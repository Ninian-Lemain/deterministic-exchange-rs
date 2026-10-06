use crate::{
    Command, EngineConfig, EngineState, Health, RouteTable, RoutedCheckpointError, RoutedEngine,
    RoutedEngineError, SequenceNumber, SessionConfig, SessionError, SessionEvent, SessionState,
    ShardId, Transition,
};
use hft_io::RxFrame;
use hft_recovery::Snapshot;
use hft_session::SessionStateMachine;
use hft_wire::parse_message;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionAdmissionError {
    Session(SessionError),
    Routed(RoutedEngineError),
    /// Command sequence advancement requires successful journaled application.
    CommandEventRequiresAdmission,
}

/// Owns session sequencing and routed application as one admission boundary.
/// The caller supplies a transport sequence independently of the command's
/// per-instrument sequence. No external venue protocol or retransmission is implied.
/// Session state and deadlines commit only when journaled application succeeds.
pub struct SessionEngine<
    'storage,
    const SHARDS: usize,
    const ACCOUNTS: usize,
    const RISK_ORDERS: usize,
    const LEVELS: usize,
    const ORDERS: usize,
    const REPORTS: usize,
    const BATCH: usize,
    const EVENTS: usize,
> {
    session: SessionStateMachine,
    engines: RoutedEngine<
        'storage,
        SHARDS,
        ACCOUNTS,
        RISK_ORDERS,
        LEVELS,
        ORDERS,
        REPORTS,
        BATCH,
        EVENTS,
    >,
}

impl<
    'storage,
    const SHARDS: usize,
    const ACCOUNTS: usize,
    const RISK_ORDERS: usize,
    const LEVELS: usize,
    const ORDERS: usize,
    const REPORTS: usize,
    const BATCH: usize,
    const EVENTS: usize,
> SessionEngine<'storage, SHARDS, ACCOUNTS, RISK_ORDERS, LEVELS, ORDERS, REPORTS, BATCH, EVENTS>
{
    #[must_use]
    pub fn new(
        config: SessionConfig,
        engines: RoutedEngine<
            'storage,
            SHARDS,
            ACCOUNTS,
            RISK_ORDERS,
            LEVELS,
            ORDERS,
            REPORTS,
            BATCH,
            EVENTS,
        >,
    ) -> Self {
        Self {
            session: SessionStateMachine::new(config),
            engines,
        }
    }

    #[must_use]
    pub const fn state(&self) -> SessionState {
        self.session.state()
    }

    #[must_use]
    pub const fn expected_sequence(&self) -> SequenceNumber {
        self.session.expected_sequence()
    }

    #[must_use]
    pub const fn deadline(&self) -> Option<u64> {
        self.session.deadline()
    }

    #[must_use]
    pub const fn routes(&self) -> &RouteTable<SHARDS> {
        self.engines.routes()
    }

    /// Drives lifecycle events using caller-supplied virtual time.
    ///
    /// # Errors
    ///
    /// Rejects command events and invalid transitions without changing state.
    pub fn handle_session(
        &mut self,
        event: SessionEvent,
        now: u64,
    ) -> Result<Transition, SessionAdmissionError> {
        if matches!(event, SessionEvent::Command { .. }) {
            return Err(SessionAdmissionError::CommandEventRequiresAdmission);
        }
        let mut staged = self.session.clone();
        let transition = staged
            .handle(event, now)
            .map_err(SessionAdmissionError::Session)?;
        self.session = staged;
        if transition.state == SessionState::Failed {
            self.engines.stop_admission();
        }
        Ok(transition)
    }

    /// # Errors
    ///
    /// Timer arithmetic failures change no state.
    pub fn tick(&mut self, now: u64) -> Result<Transition, SessionAdmissionError> {
        let mut staged = self.session.clone();
        let transition = staged.tick(now).map_err(SessionAdmissionError::Session)?;
        self.session = staged;
        if transition.state == SessionState::Failed {
            self.engines.stop_admission();
        }
        Ok(transition)
    }

    /// # Errors
    ///
    /// Parse, session, route, and overload refusals leave the session unchanged.
    /// Retry overload with the same transport sequence and command bytes.
    pub fn process_frame(
        &mut self,
        session_sequence: SequenceNumber,
        frame: &RxFrame<'_>,
        now: u64,
    ) -> Result<ShardId, SessionAdmissionError> {
        let command = parse_message(frame)
            .map_err(|error| SessionAdmissionError::Routed(RoutedEngineError::Parse(error)))?
            .to_command();
        self.process_command(session_sequence, command, now)
    }

    /// Stages session admission, applies through the routed journaled engine,
    /// then commits the session. Business rejections consume both sequences.
    ///
    /// # Errors
    ///
    /// Refusals consume neither sequence. A fatal engine failure also fails the
    /// session; recovery requires a new engine. Persistence may fail concurrently
    /// after admission, as documented by the underlying engine contract.
    pub fn process_command(
        &mut self,
        session_sequence: SequenceNumber,
        command: Command,
        now: u64,
    ) -> Result<ShardId, SessionAdmissionError> {
        // A shared session stops when any owned persistence boundary has failed,
        // even if the next command targets a different, healthy instrument.
        if let Some(shard) = self.engines.routes().routes().iter().find_map(|route| {
            self.engines
                .health(route.shard_id)
                .filter(|health| health.state == EngineState::Failed)
                .map(|_| route.shard_id)
        }) {
            let _ = self.session.handle(SessionEvent::Fail, now);
            self.engines.stop_admission();
            return Err(SessionAdmissionError::Routed(RoutedEngineError::Engine {
                shard,
                error: crate::EngineError::PersistenceFailed,
            }));
        }
        let mut staged = self.session.clone();
        staged
            .handle(
                SessionEvent::Command {
                    sequence: session_sequence,
                },
                now,
            )
            .map_err(SessionAdmissionError::Session)?;
        match self.engines.process_command(command) {
            Ok(shard) => {
                self.session = staged;
                Ok(shard)
            }
            Err(error) => {
                if let RoutedEngineError::Engine { shard, .. } = error {
                    if self
                        .engines
                        .health(shard)
                        .is_some_and(|health| health.state == EngineState::Failed)
                    {
                        // Fail has no arithmetic or transition refusal.
                        let _ = self.session.handle(SessionEvent::Fail, now);
                        self.engines.stop_admission();
                    }
                }
                Err(SessionAdmissionError::Routed(error))
            }
        }
    }

    #[must_use]
    pub fn health(&self, shard: ShardId) -> Option<Health> {
        self.engines.health(shard)
    }

    #[must_use]
    pub fn configuration(&self, shard: ShardId) -> Option<&EngineConfig> {
        self.engines.configuration(shard)
    }

    pub fn stop_admission(&mut self) {
        self.engines.stop_admission();
    }

    /// # Errors
    ///
    /// Rejects unknown shards or any engine checkpoint refusal.
    pub fn snapshot(&self, shard: ShardId) -> Result<Snapshot, RoutedCheckpointError> {
        self.engines.snapshot(shard)
    }
}
