#![forbid(unsafe_code)]

use hft_engine::{
    EngineBuilder, EngineError, EngineParts, EngineState, EngineStorage, Event, InstrumentRoute,
    RouteTable, RoutedEngine, RoutedEngineBuildError, RoutedEngineError, SessionAdmissionError,
    SessionConfig, SessionEngine, SessionError, SessionEvent, SessionState, ShardId,
};
use hft_events::EventEngineError;
use hft_gateway::GatewayError;
use hft_io::RxFrame;
use hft_journal::{JournalError, RING_CAPACITY};
use hft_risk::RiskLimits;
use hft_types::{
    AccountId, CancelOrder, Command, InstrumentId, PriceTicks, Quantity, SequenceNumber,
};

type Parts<'a> = EngineParts<'a, 2, 16, 4, 4, 4, 6, 2>;
type Service<'a> = SessionEngine<'a, 2, 2, 16, 4, 4, 4, 6, 2>;

fn parts(storage: &mut EngineStorage<6, 2>, instrument: u32) -> Parts<'_> {
    EngineBuilder::<2, 16, 4, 4, 4>::new(
        InstrumentId(instrument),
        &[(
            AccountId(1),
            RiskLimits {
                max_quantity: Quantity(100),
                max_notional: 100_000,
                max_abs_position: Quantity(1_000),
                max_open_orders: 16,
                minimum_price: PriceTicks(1),
                maximum_price: PriceTicks(1_000),
            },
        )],
    )
    .expect("builder")
    .build(storage)
    .expect("parts")
}

fn routes() -> RouteTable<2> {
    RouteTable::try_new([
        InstrumentRoute {
            instrument_id: InstrumentId(22),
            shard_id: ShardId(1),
        },
        InstrumentRoute {
            instrument_id: InstrumentId(11),
            shard_id: ShardId(0),
        },
    ])
    .expect("routes")
}

fn cancel(instrument: u32, sequence: u64) -> Command {
    Command::CancelOrder(CancelOrder {
        instrument_id: InstrumentId(instrument),
        account_id: AccountId(1),
        order_id: hft_types::OrderId(99),
        sequence: SequenceNumber(sequence),
    })
}

fn activate(service: &mut Service<'_>) {
    service
        .handle_session(SessionEvent::Connect, 0)
        .expect("connect");
    service
        .handle_session(SessionEvent::LogonSent, 0)
        .expect("logon");
    service
        .handle_session(
            SessionEvent::LogonAccepted {
                first_sequence: SequenceNumber(1),
            },
            0,
        )
        .expect("active");
}

#[test]
fn route_validation_and_independent_journaled_sequences() {
    // Execute native integration here; instrumented queues belong in a Loom model.
    if hft_spsc::IS_LOOM_BUILD {
        return;
    }
    let mut zero = EngineStorage::try_new().expect("storage");
    let mut one = EngineStorage::try_new().expect("storage");
    let mut zero_parts = parts(&mut zero, 11);
    let mut one_parts = parts(&mut one, 22);
    let mut engine =
        RoutedEngine::try_new(routes(), [zero_parts.engine, one_parts.engine]).expect("routed");
    assert_eq!(
        engine.process_command(cancel(33, 1)),
        Err(RoutedEngineError::UnknownInstrument(InstrumentId(33)))
    );
    assert_eq!(engine.process_command(cancel(11, 1)), Ok(ShardId(0)));
    assert_eq!(
        engine.health(ShardId(0)).expect("health").next_sequence,
        SequenceNumber(2)
    );
    assert_eq!(
        engine.health(ShardId(1)).expect("health").next_sequence,
        SequenceNumber(1)
    );
    assert_eq!(engine.process_command(cancel(22, 1)), Ok(ShardId(1)));
    for events in [&mut zero_parts.events, &mut one_parts.events] {
        let batch = events.try_pop().expect("business rejection batch");
        assert!(matches!(batch.iter().next(), Some(Event::Rejected(_))));
    }
    assert_eq!(
        zero_parts.journal.read().expect("journal").sequence(),
        SequenceNumber(1)
    );
    assert_eq!(
        one_parts.journal.read().expect("journal").sequence(),
        SequenceNumber(1)
    );
    engine.stop_admission();
    assert_eq!(
        engine.health(ShardId(0)).expect("health").state,
        EngineState::Stopping
    );
    assert_eq!(
        engine.health(ShardId(1)).expect("health").state,
        EngineState::Stopping
    );
}

#[test]
fn route_constructor_refuses_swapped_engines() {
    // Execute native integration here; instrumented queues belong in a Loom model.
    if hft_spsc::IS_LOOM_BUILD {
        return;
    }
    let mut zero = EngineStorage::try_new().expect("storage");
    let mut one = EngineStorage::try_new().expect("storage");
    let zero_parts = parts(&mut zero, 22);
    let one_parts = parts(&mut one, 11);
    assert!(matches!(
        RoutedEngine::try_new(routes(), [zero_parts.engine, one_parts.engine]),
        Err(RoutedEngineBuildError::InstrumentMismatch {
            shard: ShardId(0),
            ..
        })
    ));
}

#[test]
fn session_overload_retry_keeps_transport_and_instrument_sequences() {
    // Execute native integration here; instrumented queues belong in a Loom model.
    if hft_spsc::IS_LOOM_BUILD {
        return;
    }
    let mut zero = EngineStorage::try_new().expect("storage");
    let mut one = EngineStorage::try_new().expect("storage");
    let mut zero_parts = parts(&mut zero, 11);
    let one_parts = parts(&mut one, 22);
    let routed =
        RoutedEngine::try_new(routes(), [zero_parts.engine, one_parts.engine]).expect("routed");
    let mut service = SessionEngine::new(SessionConfig::default(), routed);
    assert!(matches!(
        service.process_command(SequenceNumber(1), cancel(11, 1), 0),
        Err(SessionAdmissionError::Session(_))
    ));
    activate(&mut service);
    assert_eq!(
        service.handle_session(
            SessionEvent::Command {
                sequence: SequenceNumber(1)
            },
            1
        ),
        Err(SessionAdmissionError::CommandEventRequiresAdmission)
    );
    for sequence in 1..=2 {
        service
            .process_command(SequenceNumber(sequence), cancel(11, sequence), sequence)
            .expect("fill events");
    }
    let deadline = service.deadline();
    let command = cancel(11, 3);
    assert_eq!(
        service.process_command(SequenceNumber(3), command, 10),
        Err(SessionAdmissionError::Routed(RoutedEngineError::Engine {
            shard: ShardId(0),
            error: EngineError::Admission(EventEngineError::Backpressured)
        }))
    );
    assert_eq!(service.expected_sequence(), SequenceNumber(3));
    assert_eq!(service.deadline(), deadline);
    // A saturated instrument does not block another instrument's independent sequence.
    assert_eq!(
        service.process_command(SequenceNumber(3), cancel(22, 1), 11),
        Ok(ShardId(1))
    );
    assert_eq!(service.expected_sequence(), SequenceNumber(4));
    assert!(zero_parts.events.try_pop().is_some());
    assert_eq!(
        service.process_command(SequenceNumber(4), command, 12),
        Ok(ShardId(0))
    );
    assert_eq!(service.expected_sequence(), SequenceNumber(5));
    assert_eq!(
        service.health(ShardId(0)).expect("health").next_sequence,
        SequenceNumber(4)
    );
    assert!(matches!(
        service.process_command(SequenceNumber(4), cancel(11, 4), 13),
        Err(SessionAdmissionError::Session(
            SessionError::DuplicateSequence { .. }
        ))
    ));
    service.stop_admission();
}

#[test]
fn session_validation_and_timer_failures_leave_admission_unchanged() {
    // Execute native integration here; instrumented queues belong in a Loom model.
    if hft_spsc::IS_LOOM_BUILD {
        return;
    }
    let mut zero = EngineStorage::try_new().expect("storage");
    let mut one = EngineStorage::try_new().expect("storage");
    let zero_parts = parts(&mut zero, 11);
    let one_parts = parts(&mut one, 22);
    let routed =
        RoutedEngine::try_new(routes(), [zero_parts.engine, one_parts.engine]).expect("routed");
    let mut service = SessionEngine::new(SessionConfig::default(), routed);
    activate(&mut service);
    let deadline = service.deadline();
    assert_eq!(
        service.process_command(SequenceNumber(1), cancel(33, 1), 10),
        Err(SessionAdmissionError::Routed(
            RoutedEngineError::UnknownInstrument(InstrumentId(33))
        ))
    );
    assert!(matches!(
        service.process_command(SequenceNumber(1), cancel(11, 2), 10),
        Err(SessionAdmissionError::Routed(RoutedEngineError::Engine {
            error: EngineError::Admission(EventEngineError::Gateway(GatewayError::Sequence { .. })),
            ..
        }))
    ));
    assert!(matches!(
        service.process_frame(SequenceNumber(1), &RxFrame::from_bytes(&[]), 10),
        Err(SessionAdmissionError::Routed(RoutedEngineError::Parse(_)))
    ));
    assert_eq!(
        service.process_command(SequenceNumber(1), cancel(11, 1), u64::MAX),
        Err(SessionAdmissionError::Session(
            SessionError::ArithmeticOverflow
        ))
    );
    assert_eq!(
        service.tick(u64::MAX),
        Err(SessionAdmissionError::Session(
            SessionError::ArithmeticOverflow
        ))
    );
    assert_eq!(service.deadline(), deadline);
    assert_eq!(service.expected_sequence(), SequenceNumber(1));
    assert_eq!(service.state(), SessionState::Active);
    assert_eq!(
        service.health(ShardId(0)).expect("health").next_sequence,
        SequenceNumber(1)
    );
}

#[test]
fn session_journal_pressure_retries_without_consuming_sequence_or_timer() {
    // Execute native integration here; instrumented queues belong in a Loom model.
    if hft_spsc::IS_LOOM_BUILD {
        return;
    }
    let mut zero = EngineStorage::try_new().expect("storage");
    let mut one = EngineStorage::try_new().expect("storage");
    let mut zero_parts = parts(&mut zero, 11);
    let one_parts = parts(&mut one, 22);
    let routed =
        RoutedEngine::try_new(routes(), [zero_parts.engine, one_parts.engine]).expect("routed");
    let mut service = SessionEngine::new(SessionConfig::default(), routed);
    activate(&mut service);
    for sequence in 1..=u64::try_from(RING_CAPACITY).expect("capacity") {
        service
            .process_command(SequenceNumber(sequence), cancel(11, sequence), sequence)
            .expect("journal fill");
        assert!(zero_parts.events.try_pop().is_some());
    }
    let expected = service.expected_sequence();
    let deadline = service.deadline();
    let command = cancel(11, expected.0);
    assert_eq!(
        service.process_command(expected, command, 2_000),
        Err(SessionAdmissionError::Routed(RoutedEngineError::Engine {
            shard: ShardId(0),
            error: EngineError::Journal(JournalError::Saturated)
        }))
    );
    assert_eq!(service.expected_sequence(), expected);
    assert_eq!(service.deadline(), deadline);
    assert!(zero_parts.events.try_pop().is_none());
    assert!(zero_parts.journal.read().is_ok());
    assert_eq!(
        service.process_command(expected, command, 2_001),
        Ok(ShardId(0))
    );
}

#[test]
fn persistence_loss_fails_session_and_closes_every_shard() {
    // Execute native integration here; instrumented queues belong in a Loom model.
    if hft_spsc::IS_LOOM_BUILD {
        return;
    }
    let mut zero = EngineStorage::try_new().expect("storage");
    let mut one = EngineStorage::try_new().expect("storage");
    let zero_parts = parts(&mut zero, 11);
    let one_parts = parts(&mut one, 22);
    let routed =
        RoutedEngine::try_new(routes(), [zero_parts.engine, one_parts.engine]).expect("routed");
    let mut service = SessionEngine::new(SessionConfig::default(), routed);
    activate(&mut service);
    drop(zero_parts.journal);
    assert_eq!(
        service.process_command(SequenceNumber(1), cancel(22, 1), 1),
        Err(SessionAdmissionError::Routed(RoutedEngineError::Engine {
            shard: ShardId(0),
            error: EngineError::PersistenceFailed
        }))
    );
    assert_eq!(service.state(), SessionState::Failed);
    assert_eq!(
        service.health(ShardId(0)).expect("health").state,
        EngineState::Failed
    );
    assert_eq!(
        service.health(ShardId(1)).expect("health").state,
        EngineState::Stopping
    );
    assert_eq!(
        service.health(ShardId(1)).expect("health").next_sequence,
        SequenceNumber(1)
    );
    assert!(
        service
            .process_command(SequenceNumber(1), cancel(22, 1), 2)
            .is_err()
    );
}

#[test]
fn recovery_admission_journals_frame_and_preserves_risk_rejection() {
    // Execute native integration here; instrumented queues belong in a Loom model.
    if hft_spsc::IS_LOOM_BUILD {
        return;
    }
    let mut zero = EngineStorage::try_new().expect("storage");
    let mut one = EngineStorage::try_new().expect("storage");
    let mut zero_parts = parts(&mut zero, 11);
    let one_parts = parts(&mut one, 22);
    let routed =
        RoutedEngine::try_new(routes(), [zero_parts.engine, one_parts.engine]).expect("routed");
    let mut service = SessionEngine::new(SessionConfig::default(), routed);
    activate(&mut service);
    service.tick(50).expect("recovery");
    assert_eq!(service.state(), SessionState::Recovering);
    let order = hft_types::NewOrder {
        instrument_id: InstrumentId(11),
        account_id: AccountId(1),
        order_id: hft_types::OrderId(1),
        sequence: SequenceNumber(1),
        price: PriceTicks(100),
        quantity: Quantity(101),
        side: hft_types::Side::Buy,
        time_in_force: hft_types::TimeInForce::Gtc,
    };
    let bytes = hft_wire::encode_new_order(order);
    assert_eq!(
        service.process_frame(SequenceNumber(1), &RxFrame::from_bytes(&bytes), 60),
        Ok(ShardId(0))
    );
    assert_eq!(service.state(), SessionState::Active);
    assert_eq!(service.expected_sequence(), SequenceNumber(2));
    assert_eq!(service.deadline(), Some(110));
    let record = zero_parts.journal.read().expect("journal");
    assert_eq!(record.sequence(), SequenceNumber(1));
    assert_eq!(record.slice(), &bytes);
    let batch = zero_parts.events.try_pop().expect("rejected events");
    assert!(matches!(batch.iter().next(), Some(Event::Rejected(_))));
    assert_eq!(
        service.health(ShardId(0)).expect("health").next_sequence,
        SequenceNumber(2)
    );
    service.tick(110).expect("recovering again");
    service.tick(160).expect("terminal timeout");
    assert_eq!(service.state(), SessionState::Failed);
    assert_eq!(
        service.health(ShardId(0)).expect("health").state,
        EngineState::Stopping
    );
    assert_eq!(
        service.health(ShardId(1)).expect("health").state,
        EngineState::Stopping
    );
}

#[test]
fn exhausted_instrument_fails_combined_admission_without_consuming_transport() {
    // Execute native integration here; instrumented queues belong in a Loom model.
    if hft_spsc::IS_LOOM_BUILD {
        return;
    }
    let mut risk = hft_risk::RiskEngine::<2, 16>::new();
    risk.register_account(
        AccountId(1),
        RiskLimits {
            max_quantity: Quantity(100),
            max_notional: 100_000,
            max_abs_position: Quantity(1_000),
            max_open_orders: 16,
            minimum_price: PriceTicks(1),
            maximum_price: PriceTicks(1_000),
        },
    )
    .expect("account");
    let mut state = hft_gateway::Gateway::<2, 16, 4, 4>::new(risk, InstrumentId(11)).export_state();
    state.expected_sequence = SequenceNumber(u64::MAX);
    let gateway = hft_gateway::Gateway::<2, 16, 4, 4>::from_state(&state).expect("state");
    let snapshot = hft_recovery::encode_snapshot(&gateway, u64::MAX - 1).expect("snapshot");
    let mut zero = EngineStorage::<6, 2>::try_new().expect("storage");
    let mut one = EngineStorage::try_new().expect("storage");
    let mut zero_parts =
        EngineBuilder::<2, 16, 4, 4, 4>::restore(InstrumentId(11), snapshot.bytes(), &[])
            .expect("restore")
            .build(&mut zero)
            .expect("parts");
    let one_parts = parts(&mut one, 22);
    let routed =
        RoutedEngine::try_new(routes(), [zero_parts.engine, one_parts.engine]).expect("routed");
    let mut service = SessionEngine::new(SessionConfig::default(), routed);
    activate(&mut service);
    assert!(matches!(
        service.process_command(SequenceNumber(1), cancel(11, u64::MAX), 1),
        Err(SessionAdmissionError::Routed(RoutedEngineError::Engine {
            error: EngineError::Admission(EventEngineError::Gateway(GatewayError::RiskState(
                hft_types::RejectReason::ArithmeticOverflow
            ))),
            ..
        }))
    ));
    assert_eq!(service.state(), SessionState::Failed);
    assert_eq!(service.expected_sequence(), SequenceNumber(1));
    assert_eq!(
        service.health(ShardId(1)).expect("health").state,
        EngineState::Stopping
    );
    assert!(zero_parts.events.try_pop().is_none());
    assert!(matches!(
        zero_parts.journal.read(),
        Err(hft_journal::ReadError::Empty)
    ));
}
