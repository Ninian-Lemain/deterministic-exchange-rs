use hft_session::retransmit::{MAX_FRAME, RetainError, RetransmitBuffer};
use hft_session::{SessionConfig, SessionEvent, SessionState, SessionStateMachine};
use hft_types::SequenceNumber;

fn frame_bytes(id: u64) -> [u8; MAX_FRAME] {
    let mut bytes = [0_u8; MAX_FRAME];
    let tag = id.to_be_bytes();
    bytes[..8].copy_from_slice(&tag);
    bytes[8] = 0xAB;
    bytes
}

#[test]
fn retention_is_in_order_and_exhausts_explicitly() {
    let mut buffer = RetransmitBuffer::new(3);
    assert!(buffer.is_empty());
    assert_eq!(buffer.next_sequence(), 1);

    buffer.retain(SequenceNumber(1), &frame_bytes(1)).unwrap();
    buffer.retain(SequenceNumber(2), &frame_bytes(2)).unwrap();
    buffer.retain(SequenceNumber(3), &frame_bytes(3)).unwrap();
    assert_eq!(buffer.len(), 3);
    assert_eq!(buffer.remaining_capacity(), 0);

    // Exhaustion is explicit: a fourth admission is refused, not dropped.
    assert_eq!(
        buffer.retain(SequenceNumber(4), &frame_bytes(4)),
        Err(RetainError::Full)
    );
    // Out-of-order or duplicated retention is refused as well.
    assert_eq!(
        buffer.retain(SequenceNumber(2), &frame_bytes(2)),
        Err(RetainError::NotInOrder {
            expected: SequenceNumber(4),
            received: SequenceNumber(2),
        })
    );
}

#[test]
fn confirmation_drops_only_the_confirmed_prefix() {
    let mut buffer = RetransmitBuffer::new(8);
    for id in 1..=5_u64 {
        buffer.retain(SequenceNumber(id), &frame_bytes(id)).unwrap();
    }

    assert_eq!(buffer.confirm_through(3), 3);
    assert_eq!(buffer.len(), 2);

    // Everything from 1 upward is still addressable; delivered frames are
    // skipped by `since`.
    let retransmit: Vec<u64> = buffer.since(1).map(|frame| frame.sequence.0).collect();
    assert_eq!(retransmit, [4, 5]);

    let retransmit: Vec<u64> = buffer.since(5).map(|frame| frame.sequence.0).collect();
    assert_eq!(retransmit, [5]);
}

#[test]
fn retransmitted_payloads_are_bit_identical() {
    let mut buffer = RetransmitBuffer::new(4);
    for id in 1..=4_u64 {
        buffer.retain(SequenceNumber(id), &frame_bytes(id)).unwrap();
    }
    for (offset, frame) in buffer.since(1).enumerate() {
        let id = offset as u64 + 1;
        assert_eq!(frame.sequence.0, id);
        assert_eq!(&frame.slice()[..8], &id.to_be_bytes());
        assert_eq!(frame.slice()[8], 0xAB);
    }
}

/// Deterministic timeout/reconnect: the state machine drops to Recovering,
/// the retained window is replayed from the first unconfirmed sequence, and
/// the confirming command restores Active. Delayed duplicates of already
/// confirmed frames cannot re-enter.
#[test]
fn timeout_replays_retained_window_then_recovers() {
    let config = SessionConfig {
        logon_timeout_ticks: 100,
        heartbeat_timeout_ticks: 50,
    };
    let mut session = SessionStateMachine::new(config);
    let mut buffer = RetransmitBuffer::new(16);

    session.handle(SessionEvent::Connect, 0).unwrap();
    session.handle(SessionEvent::LogonSent, 0).unwrap();
    session
        .handle(
            SessionEvent::LogonAccepted {
                first_sequence: SequenceNumber(1),
            },
            0,
        )
        .unwrap();

    for seq in 1_u64..=6 {
        session
            .handle(
                SessionEvent::Command {
                    sequence: SequenceNumber(seq),
                },
                seq,
            )
            .unwrap();
        buffer
            .retain(SequenceNumber(seq), &frame_bytes(seq))
            .unwrap();
    }

    // Commands kept re-arming the heartbeat deadline; tick far past the
    // last one to force the timeout deterministically.
    let fired_at = 200_u64;
    session.tick(fired_at).expect("timeout transition");
    assert_eq!(session.state(), SessionState::Recovering);

    // Replay the whole retained window (peer confirmed nothing).
    let replayed: Vec<u64> = buffer.since(1).map(|f| f.sequence.0).collect();
    assert_eq!(replayed, (1..=6).collect::<Vec<_>>());

    // Peer confirms through 6 with the first replayed command; recovery ends.
    let transition = session
        .handle(
            SessionEvent::Command {
                sequence: SequenceNumber(7),
            },
            fired_at + 10,
        )
        .expect("recovery completes");
    assert_eq!(transition.state, SessionState::Active);
    assert_eq!(session.expected_sequence(), SequenceNumber(8));
    buffer.confirm_through(6);
    assert_eq!(buffer.confirm_through(6), 0, "idempotent confirmation");

    // A delayed duplicate of confirmed sequence 6 is refused and cannot
    // duplicate the command.
    assert_eq!(
        session.handle(
            SessionEvent::Command {
                sequence: SequenceNumber(6),
            },
            fired_at + 11,
        ),
        Err(hft_session::SessionError::DuplicateSequence {
            received: SequenceNumber(6),
            expected: SequenceNumber(8),
        })
    );
}
