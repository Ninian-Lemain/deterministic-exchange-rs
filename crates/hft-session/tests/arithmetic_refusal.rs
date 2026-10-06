use hft_session::{SessionConfig, SessionError, SessionEvent, SessionState, SessionStateMachine};
use hft_types::SequenceNumber;

#[test]
fn deadline_overflow_preserves_logon_command_and_tick_state() {
    let mut session = SessionStateMachine::new(SessionConfig::default());
    session.handle(SessionEvent::Connect, 0).expect("connect");
    session.handle(SessionEvent::LogonSent, 0).expect("logon");
    let deadline = session.deadline();
    assert_eq!(
        session.handle(
            SessionEvent::LogonAccepted {
                first_sequence: SequenceNumber(9)
            },
            u64::MAX
        ),
        Err(SessionError::ArithmeticOverflow)
    );
    assert_eq!(session.state(), SessionState::Logon);
    assert_eq!(session.expected_sequence(), SequenceNumber(1));
    assert_eq!(session.deadline(), deadline);
    session
        .handle(
            SessionEvent::LogonAccepted {
                first_sequence: SequenceNumber(9),
            },
            0,
        )
        .expect("active");
    let deadline = session.deadline();
    assert_eq!(
        session.handle(
            SessionEvent::Command {
                sequence: SequenceNumber(9)
            },
            u64::MAX
        ),
        Err(SessionError::ArithmeticOverflow)
    );
    assert_eq!(session.expected_sequence(), SequenceNumber(9));
    assert_eq!(session.deadline(), deadline);
    assert_eq!(session.state(), SessionState::Active);
    assert_eq!(
        session.tick(u64::MAX),
        Err(SessionError::ArithmeticOverflow)
    );
    assert_eq!(session.state(), SessionState::Active);
    assert_eq!(session.deadline(), deadline);
    session.tick(50).expect("recovering");
    assert_eq!(
        session.handle(
            SessionEvent::Command {
                sequence: SequenceNumber(9)
            },
            u64::MAX
        ),
        Err(SessionError::ArithmeticOverflow)
    );
    assert_eq!(session.expected_sequence(), SequenceNumber(9));
    assert_eq!(session.state(), SessionState::Recovering);
    session
        .handle(
            SessionEvent::Command {
                sequence: SequenceNumber(9),
            },
            60,
        )
        .expect("retry");
    assert_eq!(session.expected_sequence(), SequenceNumber(10));
    assert_eq!(session.state(), SessionState::Active);
}
