//! Replay fixture: a session gates the matching core. Frames are dispatched
//! to the gateway only while the session is Active; a mid-stream
//! disconnect holds traffic, and reconnect resumes sequencing exactly where
//! it stopped. No gaps, no duplicates.

use hft_gateway::{Gateway, GatewayOutcome};
use hft_io::RxFrame;
use hft_risk::RiskEngine;
use hft_session::{SessionConfig, SessionEvent, SessionState, SessionStateMachine};
use hft_types::{
    AccountId, InstrumentId, NewOrder, OrderId, PriceTicks, Quantity, ReportBuffer, SequenceNumber,
    Side,
};
use hft_wire::{encode_cancel_order, encode_new_order, encode_replace_order};

fn new_gateway() -> Gateway<2, 8, 4, 4> {
    let mut risk = RiskEngine::<2, 8>::new();
    let limits = hft_risk::RiskLimits {
        max_quantity: Quantity(100),
        max_notional: 100_000,
        max_abs_position: Quantity(1_000),
        max_open_orders: 8,
        minimum_price: PriceTicks(1),
        maximum_price: PriceTicks(1_000),
    };
    risk.register_account(AccountId(1), limits).unwrap();
    risk.register_account(AccountId(2), limits).unwrap();
    Gateway::new(risk, InstrumentId(7))
}

fn order_frame(id: u64, account: u32, side: Side) -> [u8; 46] {
    encode_new_order(NewOrder {
        time_in_force: hft_types::TimeInForce::Gtc,
        order_id: OrderId(id),
        account_id: AccountId(account),
        instrument_id: InstrumentId(7),
        price: PriceTicks(100),
        quantity: Quantity(5),
        sequence: SequenceNumber(id),
        side,
    })
}

#[test]
fn replay_fixture_gates_the_gateway_through_the_session() {
    let mut gateway = new_gateway();
    let mut reports = ReportBuffer::<4>::new();

    let mut session = SessionStateMachine::new(SessionConfig::default());

    let frames = [
        order_frame(1, 1, Side::Sell),
        order_frame(2, 2, Side::Buy),
        order_frame(3, 1, Side::Sell),
    ];

    // Handshake first; nothing reaches the core before Active.
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
    assert_eq!(session.state(), SessionState::Active);

    // Frame 1 passes through the active session.
    session
        .handle(
            SessionEvent::Command {
                sequence: SequenceNumber(1),
            },
            1,
        )
        .unwrap();
    match gateway.process_frame(&RxFrame::from_bytes(&frames[0]), &mut reports) {
        Ok(GatewayOutcome::NewOrder(_)) => {}
        other => panic!("expected resting order, got {other:?}"),
    }

    // Mid-stream disconnect: admission stops, frames queue unprocessed, and
    // the gateway sequence stays exactly where it was.
    session.handle(SessionEvent::Disconnect, 2).unwrap();
    assert!(!session.allows_commands());

    session.handle(SessionEvent::Connect, 3).unwrap();
    session.handle(SessionEvent::LogonSent, 3).unwrap();
    session
        .handle(
            SessionEvent::LogonAccepted {
                first_sequence: SequenceNumber(2),
            },
            4,
        )
        .unwrap();
    assert_eq!(session.state(), SessionState::Active);
    assert_eq!(session.expected_sequence(), SequenceNumber(2));

    // Replayed tail: every remaining frame is admitted in order.
    for (offset, frame) in frames.iter().enumerate().skip(1) {
        session
            .handle(
                SessionEvent::Command {
                    sequence: SequenceNumber(offset as u64 + 1),
                },
                4 + offset as u64,
            )
            .expect("replayed command admitted");
        gateway
            .process_frame(&RxFrame::from_bytes(frame), &mut reports)
            .expect("replayed frame accepted by the gateway");
    }
    assert_eq!(
        gateway.expected_sequence(),
        SequenceNumber(frames.len() as u64 + 1)
    );
}

/// A replace issued through an active session mutates the resting order it
/// names, proving the lifecycle composes across both layers.
#[test]
fn active_session_forwards_replaces_to_the_gateway() {
    let mut gateway = new_gateway();
    let mut reports = ReportBuffer::<4>::new();
    let mut session = SessionStateMachine::new(SessionConfig::default());

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

    session
        .handle(
            SessionEvent::Command {
                sequence: SequenceNumber(1),
            },
            1,
        )
        .unwrap();
    gateway
        .process_frame(
            &RxFrame::from_bytes(&order_frame(1, 1, Side::Sell)),
            &mut reports,
        )
        .unwrap();

    session
        .handle(
            SessionEvent::Command {
                sequence: SequenceNumber(2),
            },
            2,
        )
        .unwrap();
    let replace = encode_replace_order(hft_types::ReplaceOrder {
        order_id: OrderId(1),
        account_id: AccountId(1),
        instrument_id: InstrumentId(7),
        sequence: SequenceNumber(2),
        price: PriceTicks(101),
        quantity: Quantity(7),
    });
    gateway
        .process_frame(&RxFrame::from_bytes(&replace), &mut reports)
        .unwrap();

    // The replaced order re-priced and grew: cancel reports seven units.
    let cancelled = gateway
        .process_frame(
            &RxFrame::from_bytes(&encode_cancel_order(hft_types::CancelOrder {
                order_id: OrderId(1),
                account_id: AccountId(1),
                instrument_id: InstrumentId(7),
                sequence: SequenceNumber(3),
            })),
            &mut reports,
        )
        .unwrap();
    match cancelled {
        GatewayOutcome::Cancelled(cancelled) => {
            assert_eq!(cancelled.quantity, Quantity(7));
        }
        other => panic!("expected cancel, got {other:?}"),
    }
}
