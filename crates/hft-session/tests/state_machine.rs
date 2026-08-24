use hft_session::{SessionConfig, SessionError, SessionEvent, SessionState, SessionStateMachine};
use hft_types::SequenceNumber;

fn machine() -> SessionStateMachine {
    let mut session = SessionStateMachine::new(SessionConfig {
        logon_timeout_ticks: 100,
        heartbeat_timeout_ticks: 50,
    });
    session.handle(SessionEvent::Connect, 0).expect("connect");
    session
        .handle(SessionEvent::LogonSent, 0)
        .expect("logon sent");
    session
        .handle(
            SessionEvent::LogonAccepted {
                first_sequence: SequenceNumber(1),
            },
            0,
        )
        .expect("logon accepted");
    assert_eq!(session.state(), SessionState::Active);
    session
}

#[test]
fn happy_path_reaches_active_and_processes_commands() {
    let mut session = machine();
    for seq in 1_u64..=4 {
        let transition = session
            .handle(
                SessionEvent::Command {
                    sequence: SequenceNumber(seq),
                },
                seq,
            )
            .expect("in-sequence command");
        assert_eq!(transition.state, SessionState::Active);
        assert_eq!(
            session.expected_sequence(),
            SequenceNumber(seq + 1),
            "sequence advances only on acceptance"
        );
    }
}

#[test]
fn gap_fails_closed_without_advancing() {
    let mut session = machine();
    let before = (
        session.state(),
        session.expected_sequence(),
        session.deadline(),
    );
    assert_eq!(
        session.handle(
            SessionEvent::Command {
                sequence: SequenceNumber(3),
            },
            1,
        ),
        Err(SessionError::Gap {
            expected: SequenceNumber(1),
            received: SequenceNumber(3),
        })
    );
    assert_eq!(before.0, session.state());
    assert_eq!(before.1, session.expected_sequence());
    assert_eq!(before.2, session.deadline());
}

#[test]
fn duplicate_fails_closed_without_advancing() {
    let mut session = machine();
    session
        .handle(
            SessionEvent::Command {
                sequence: SequenceNumber(1),
            },
            1,
        )
        .expect("first command");
    let expected = session.expected_sequence();
    assert_eq!(
        session.handle(
            SessionEvent::Command {
                sequence: SequenceNumber(1),
            },
            2,
        ),
        Err(SessionError::DuplicateSequence {
            received: SequenceNumber(1),
            expected,
        })
    );
    assert_eq!(session.expected_sequence(), expected);
}

#[test]
fn commands_outside_active_are_invalid_transitions() {
    let mut session = SessionStateMachine::new(SessionConfig::default());
    let error = session.handle(
        SessionEvent::Command {
            sequence: SequenceNumber(1),
        },
        0,
    );
    assert_eq!(
        error,
        Err(SessionError::InvalidTransition {
            state: SessionState::Disconnected,
            event: "command",
        })
    );
    assert_eq!(session.state(), SessionState::Disconnected);
    assert_eq!(session.expected_sequence(), SequenceNumber(1));
}

#[test]
fn logon_timeout_fails_the_session() {
    let mut session = SessionStateMachine::new(SessionConfig {
        logon_timeout_ticks: 100,
        heartbeat_timeout_ticks: 50,
    });
    session.handle(SessionEvent::Connect, 0).unwrap();
    session.handle(SessionEvent::LogonSent, 0).unwrap();
    assert_eq!(
        session.deadline(),
        Some(100),
        "logon deadline is armed at send"
    );

    let before = session.state();
    session.tick(99).unwrap();
    assert_eq!(session.state(), before, "deadline has not fired yet");

    let transition = session.tick(100).unwrap();
    assert_eq!(transition.state, SessionState::Failed);
    assert_eq!(session.state(), SessionState::Failed);

    // A failed session is terminal in both directions.
    assert!(session.handle(SessionEvent::Connect, 200).is_err());
    assert_eq!(
        session.handle(SessionEvent::Disconnect, 200),
        Err(SessionError::InvalidTransition {
            state: SessionState::Failed,
            event: "disconnect",
        })
    );
}

#[test]
fn heartbeat_timeout_drops_to_recovering_then_recovers_on_command() {
    let mut session = machine();
    // Arm the heartbeat deadline at t=50.
    session.tick(49).unwrap();
    assert_eq!(session.state(), SessionState::Active);

    let transition = session.tick(50).unwrap();
    assert_eq!(transition.state, SessionState::Recovering);
    // Recovery is bounded: a second window is armed immediately.
    assert_eq!(transition.deadline, Some(100));
    assert!(session.allows_commands());

    // The very next in-sequence command restores Active with a fresh
    // heartbeat deadline; no sequence was lost during recovery.
    let transition = session
        .handle(
            SessionEvent::Command {
                sequence: SequenceNumber(1),
            },
            60,
        )
        .expect("recovery completes on next command");
    assert_eq!(transition.state, SessionState::Active);
    assert_eq!(transition.deadline, Some(111));
    assert_eq!(session.expected_sequence(), SequenceNumber(2));

    // Heartbeats also keep the session alive without commands.
    session
        .handle(SessionEvent::HeartbeatReceived, 150)
        .unwrap();
    assert_eq!(session.deadline(), Some(200));
}

#[test]
fn second_consecutive_heartbeat_timeout_fails() {
    let mut session = machine();
    let transition = session.tick(50).unwrap();
    assert_eq!(transition.state, SessionState::Recovering);

    let transition = session.tick(100).unwrap();
    assert_eq!(transition.state, SessionState::Failed);
    assert_eq!(session.state(), SessionState::Failed);
    assert_eq!(session.expected_sequence(), SequenceNumber(1));
}

#[test]
fn logout_and_disconnect_paths() {
    let mut session = machine();
    session.handle(SessionEvent::LogoutSent, 10).unwrap();
    assert_eq!(session.state(), SessionState::Logout);
    // Commands are refused while logging out.
    assert!(
        session
            .handle(
                SessionEvent::Command {
                    sequence: SequenceNumber(1),
                },
                11,
            )
            .is_err()
    );
    session.handle(SessionEvent::Disconnect, 12).unwrap();
    assert_eq!(session.state(), SessionState::Disconnected);
    // Fresh session again: sequences restart at one.
    session.handle(SessionEvent::Connect, 20).unwrap();
    assert_eq!(session.expected_sequence(), SequenceNumber(1));
}
