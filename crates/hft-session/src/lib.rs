//! Session state machine: connection lifecycle and inbound sequencing
//! outside the matching core. Deterministic virtual time only. Every
//! deadline check takes `now` from the caller.
//!
//! Commands are accepted exclusively in [`SessionState::Active`]; gaps,
//! duplicates, and invalid transitions fail closed without advancing state,
//! sequence, or timers.
#![forbid(unsafe_code)]

pub mod retransmit;

use core::fmt;
use hft_types::SequenceNumber;

/// Lifecycle states, in roadmap order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionState {
    Disconnected,
    Connecting,
    Logon,
    Active,
    Recovering,
    Logout,
    Failed,
}

impl fmt::Display for SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            SessionState::Disconnected => "disconnected",
            SessionState::Connecting => "connecting",
            SessionState::Logon => "logon",
            SessionState::Active => "active",
            SessionState::Recovering => "recovering",
            SessionState::Logout => "logout",
            SessionState::Failed => "failed",
        };
        f.write_str(name)
    }
}

/// Inbound events driving the machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionEvent {
    Connect,
    LogonSent,
    LogonAccepted { first_sequence: SequenceNumber },
    Command { sequence: SequenceNumber },
    HeartbeatReceived,
    LogoutSent,
    Disconnect,
    Fail,
}

/// Every way an event can be refused. Refusal never mutates the session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionError {
    InvalidTransition {
        state: SessionState,
        event: &'static str,
    },
    Gap {
        expected: SequenceNumber,
        received: SequenceNumber,
    },
    DuplicateSequence {
        received: SequenceNumber,
        expected: SequenceNumber,
    },
    NotActive,
    ArithmeticOverflow,
}

/// Deadline configuration in virtual ticks; zero disables a deadline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionConfig {
    pub logon_timeout_ticks: u64,
    pub heartbeat_timeout_ticks: u64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            logon_timeout_ticks: 100,
            heartbeat_timeout_ticks: 50,
        }
    }
}

/// One step's outcome: the new state plus any deadline that was armed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Transition {
    pub state: SessionState,
    /// Absolute tick at which the current deadline fires, if one is armed.
    pub deadline: Option<u64>,
}

/// The machine. Sequence bookkeeping mirrors the gateway: commands must
/// arrive with exactly `next_sequence`, which advances only on acceptance.
#[derive(Debug)]
pub struct SessionStateMachine {
    state: SessionState,
    config: SessionConfig,
    next_sequence: u64,
    deadline: Option<u64>,
    /// Heartbeat timeouts suffered since the last accepted command; two in a
    /// row (Active then Recovering) fail the session.
    heartbeat_timeouts: u32,
}

impl SessionStateMachine {
    #[must_use]
    pub fn new(config: SessionConfig) -> Self {
        Self {
            state: SessionState::Disconnected,
            config,
            next_sequence: 1,
            deadline: None,
            heartbeat_timeouts: 0,
        }
    }

    #[must_use]
    pub const fn state(&self) -> SessionState {
        self.state
    }

    #[must_use]
    pub const fn expected_sequence(&self) -> SequenceNumber {
        SequenceNumber(self.next_sequence)
    }

    #[must_use]
    pub const fn deadline(&self) -> Option<u64> {
        self.deadline
    }

    #[must_use]
    pub const fn allows_commands(&self) -> bool {
        matches!(self.state, SessionState::Active | SessionState::Recovering)
    }

    /// Applies one event at virtual time `now`.
    ///
    /// # Errors
    ///
    /// Returns the precise refusal without mutating any session field.
    pub fn handle(&mut self, event: SessionEvent, now: u64) -> Result<Transition, SessionError> {
        match (self.state, event) {
            // --- connect ---
            (SessionState::Disconnected, SessionEvent::Connect) => {
                Ok(self.enter(SessionState::Connecting, None))
            }
            // --- logon handshake ---
            (SessionState::Connecting, SessionEvent::LogonSent) => {
                self.arm(SessionState::Logon, self.config.logon_timeout_ticks, now)
            }
            (SessionState::Logon, SessionEvent::LogonAccepted { first_sequence }) => {
                if first_sequence.0 == 0 {
                    return Err(SessionError::ArithmeticOverflow);
                }
                self.next_sequence = first_sequence.0;
                self.heartbeat_timeouts = 0;
                self.arm(
                    SessionState::Active,
                    self.config.heartbeat_timeout_ticks,
                    now,
                )
            }
            // --- commands ---
            (
                SessionState::Active | SessionState::Recovering,
                SessionEvent::Command { sequence },
            ) => {
                let received = sequence.0;
                if received < self.next_sequence {
                    return Err(SessionError::DuplicateSequence {
                        received: sequence,
                        expected: SequenceNumber(self.next_sequence),
                    });
                }
                if received > self.next_sequence {
                    return Err(SessionError::Gap {
                        expected: SequenceNumber(self.next_sequence),
                        received: sequence,
                    });
                }
                match self.next_sequence.checked_add(1) {
                    Some(next) => self.next_sequence = next,
                    None => return Err(SessionError::ArithmeticOverflow),
                }
                self.heartbeat_timeouts = 0;
                self.state = SessionState::Active;
                self.arm(self.state, self.config.heartbeat_timeout_ticks, now)
            }
            // --- heartbeats keep Active alive ---
            (SessionState::Active, SessionEvent::HeartbeatReceived) => self.arm(
                SessionState::Active,
                self.config.heartbeat_timeout_ticks,
                now,
            ),
            // --- logout / disconnect / fail ---
            (
                SessionState::Connecting
                | SessionState::Logon
                | SessionState::Active
                | SessionState::Recovering,
                SessionEvent::LogoutSent,
            ) => Ok(self.enter(SessionState::Logout, None)),
            (state, SessionEvent::Disconnect) if state != SessionState::Failed => {
                self.reset();
                Ok(self.enter(SessionState::Disconnected, None))
            }
            (_, SessionEvent::Fail) => {
                self.reset();
                Ok(self.enter(SessionState::Failed, None))
            }
            (state, event) => Err(SessionError::InvalidTransition {
                state,
                event: event_name(event),
            }),
        }
    }

    /// Advances virtual time. Deadlines fire exactly once when crossed:
    /// Logon timeout fails the session, an Active heartbeat timeout drops to
    /// Recovering, and a second consecutive timeout in Recovering fails it.
    ///
    /// # Errors
    ///
    /// Returns refusal only for arithmetic overflow while re-arming.
    pub fn tick(&mut self, now: u64) -> Result<Transition, SessionError> {
        let Some(deadline) = self.deadline else {
            return Ok(self.snapshot());
        };
        if now < deadline {
            return Ok(self.snapshot());
        }
        match self.state {
            SessionState::Logon => {
                self.reset();
                Ok(self.enter(SessionState::Failed, None))
            }
            SessionState::Active => {
                self.heartbeat_timeouts += 1;
                let deadline = now
                    .checked_add(self.config.heartbeat_timeout_ticks)
                    .ok_or(SessionError::ArithmeticOverflow)?;
                self.state = SessionState::Recovering;
                self.deadline = Some(deadline);
                Ok(Transition {
                    state: SessionState::Recovering,
                    deadline: self.deadline,
                })
            }
            SessionState::Recovering => {
                self.heartbeat_timeouts += 1;
                self.reset();
                Ok(self.enter(SessionState::Failed, None))
            }
            _ => Ok(self.snapshot()),
        }
    }

    fn enter(&mut self, state: SessionState, deadline: Option<u64>) -> Transition {
        self.state = state;
        self.deadline = deadline;
        self.snapshot()
    }

    fn arm(
        &mut self,
        state: SessionState,
        timeout: u64,
        now: u64,
    ) -> Result<Transition, SessionError> {
        let deadline = now
            .checked_add(timeout)
            .ok_or(SessionError::ArithmeticOverflow)?;
        self.state = state;
        self.deadline = Some(deadline);
        Ok(self.snapshot())
    }

    fn reset(&mut self) {
        self.next_sequence = 1;
        self.deadline = None;
        self.heartbeat_timeouts = 0;
    }

    fn snapshot(&self) -> Transition {
        Transition {
            state: self.state,
            deadline: self.deadline,
        }
    }
}

fn event_name(event: SessionEvent) -> &'static str {
    match event {
        SessionEvent::Connect => "connect",
        SessionEvent::LogonSent => "logon_sent",
        SessionEvent::LogonAccepted { .. } => "logon_accepted",
        SessionEvent::Command { .. } => "command",
        SessionEvent::HeartbeatReceived => "heartbeat_received",
        SessionEvent::LogoutSent => "logout_sent",
        SessionEvent::Disconnect => "disconnect",
        SessionEvent::Fail => "fail",
    }
}
