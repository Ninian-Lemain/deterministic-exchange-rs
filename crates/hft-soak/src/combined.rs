//! Repeated faults across the owned session, route, event and journal boundary.
use crate::Seed;
use crate::events::validate_batch;
use hft_engine::{
    EngineBuilder, EngineParts, EngineState, EngineStorage, RoutedEngine, SessionEngine,
};
use hft_gateway::Gateway;
use hft_io::RxFrame;
use hft_journal::{DurableSink, FlushPolicy, PersistenceWorker, RECORD_SIZE, RING_CAPACITY};
use hft_recovery::encode_snapshot;
use hft_risk::{RiskEngine, RiskLimits};
use hft_router::{InstrumentRoute, RouteTable, ShardId};
use hft_session::{SessionConfig, SessionEvent, SessionState};
use hft_spsc::Consumer;
use hft_types::{
    AccountId, CancelOrder, Command, InstrumentId, NewOrder, OrderId, PriceTicks, Quantity,
    SequenceNumber, Side, TimeInForce,
};
use std::io;

type Service<'a> = SessionEngine<'a, 2, 2, 16, 4, 4, 4, 6, 2>;
type Builder = EngineBuilder<2, 16, 4, 4, 4>;
type Events<'a> = Consumer<'a, hft_engine::EventBatch<6>, 2>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CombinedResult {
    pub rounds: u64,
    pub commands: u64,
    pub event_pressure: u64,
    pub journal_pressure: u64,
    pub session_refusals: u64,
    pub reconnects: u64,
    pub heartbeat_timeouts: u64,
    pub malformed_frames: u64,
    pub unknown_routes: u64,
    pub recovery_checks: u64,
    pub shutdown_races: u64,
    pub write_failures: u64,
    pub flush_failures: u64,
    pub abandoned_workers: u64,
    pub fingerprint: u64,
}

#[derive(Default)]
struct Sink {
    bytes: Vec<u8>,
    fail_write: bool,
    fail_flush: bool,
}

impl DurableSink for Sink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.fail_write {
            return Err(io::ErrorKind::Other.into());
        }
        let count = bytes.len().min(7);
        self.bytes.extend_from_slice(&bytes[..count]);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.fail_flush {
            return Err(io::ErrorKind::Other.into());
        }
        Ok(())
    }
}

fn checked<T, E: std::fmt::Debug>(value: Result<T, E>) -> Result<T, String> {
    value.map_err(|error| format!("combined service: {error:?}"))
}

fn ensure(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}

fn limits() -> RiskLimits {
    RiskLimits {
        max_quantity: Quantity(100),
        max_notional: 100_000,
        max_abs_position: Quantity(1_000_000),
        max_open_orders: 16,
        minimum_price: PriceTicks(1),
        maximum_price: PriceTicks(1_000),
    }
}

fn builder(instrument: InstrumentId) -> Result<Builder, String> {
    checked(Builder::new(
        instrument,
        &[(AccountId(1), limits()), (AccountId(2), limits())],
    ))
}

fn service(
    engines: [hft_engine::Engine<'_, 2, 16, 4, 4, 4, 6, 2>; 2],
) -> Result<Service<'_>, String> {
    let routes = checked(RouteTable::try_new([
        InstrumentRoute {
            instrument_id: InstrumentId(7),
            shard_id: ShardId(0),
        },
        InstrumentRoute {
            instrument_id: InstrumentId(8),
            shard_id: ShardId(1),
        },
    ]))?;
    let engines = checked(RoutedEngine::try_new(routes, engines))?;
    let mut service = SessionEngine::new(
        SessionConfig {
            logon_timeout_ticks: 20,
            heartbeat_timeout_ticks: 5,
        },
        engines,
    );
    activate(&mut service, SequenceNumber(1), 0)?;
    Ok(service)
}

fn activate(service: &mut Service<'_>, sequence: SequenceNumber, now: u64) -> Result<(), String> {
    for event in [
        SessionEvent::Connect,
        SessionEvent::LogonSent,
        SessionEvent::LogonAccepted {
            first_sequence: sequence,
        },
    ] {
        checked(service.handle_session(event, now))?;
    }
    Ok(())
}

fn cancel(service: &Service<'_>, shard: ShardId) -> Result<Command, String> {
    let sequence = service.health(shard).ok_or("missing shard")?.next_sequence;
    Ok(Command::CancelOrder(CancelOrder {
        instrument_id: InstrumentId(7 + u32::from(shard.0)),
        sequence,
        order_id: OrderId(u64::MAX),
        account_id: AccountId(1),
    }))
}

fn pop(
    events: &mut Events<'_>,
    command: Command,
    result: &mut CombinedResult,
) -> Result<(), String> {
    let batch = events.try_pop().ok_or("missing combined event")?;
    let summary = checked(validate_batch(
        &batch,
        command.instrument_id(),
        command.sequence(),
    ))?;
    result.fingerprint = result.fingerprint.rotate_left(7) ^ summary.fingerprint;
    result.commands += 1;
    Ok(())
}

fn admit(service: &mut Service<'_>, command: Command) -> Result<(), String> {
    admit_at(service, command, 1)
}

fn admit_at(service: &mut Service<'_>, command: Command, now: u64) -> Result<(), String> {
    checked(service.process_command(service.expected_sequence(), command, now))?;
    Ok(())
}

/// Repeats saturation, reconnect, recovery and concurrent shutdown with seeded routing.
///
/// # Errors
/// Returns an unexpected refusal, missing event, recovery difference or worker failure.
pub fn run_combined(seed: Seed, rounds: u64) -> Result<CombinedResult, String> {
    ensure(rounds > 0, "combined rounds must be nonzero")?;
    // Fixed-capacity debug fixtures exceed the Windows test-thread stack.
    std::thread::Builder::new()
        .name("combined-soak".to_owned())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || run_rounds(seed, rounds))
        .map_err(|error| format!("combined thread: {error}"))?
        .join()
        .map_err(|_| "combined soak thread panicked".to_owned())?
}

fn run_rounds(seed: Seed, rounds: u64) -> Result<CombinedResult, String> {
    let mut result = CombinedResult {
        rounds,
        fingerprint: seed.0,
        ..CombinedResult::default()
    };
    for round in 0..rounds {
        run_round(seed.0 ^ round, &mut result)?;
        for failure in 0..3 {
            run_failure(failure, &mut result)?;
        }
    }
    Ok(result)
}

fn run_round(seed: u64, result: &mut CombinedResult) -> Result<(), String> {
    let mut storage0 = checked(EngineStorage::<6, 2>::try_new())?;
    let mut storage1 = checked(EngineStorage::<6, 2>::try_new())?;
    let EngineParts {
        engine: engine0,
        events: events0,
        journal: journal0,
    } = checked(builder(InstrumentId(7))?.build(&mut storage0))?;
    let EngineParts {
        engine: engine1,
        events: events1,
        journal: journal1,
    } = checked(builder(InstrumentId(8))?.build(&mut storage1))?;
    let mut service = service([engine0, engine1])?;
    let mut events = [events0, events1];
    let mut workers = [
        checked(PersistenceWorker::<_, 16>::new(
            journal0,
            Sink::default(),
            FlushPolicy::OnShutdown,
        ))?,
        checked(PersistenceWorker::<_, 16>::new(
            journal1,
            Sink::default(),
            FlushPolicy::OnShutdown,
        ))?,
    ];
    pressure(&mut service, &mut events, &mut workers[0], result)?;
    session_faults(&mut service, &mut events, result)?;
    traffic(seed, &mut service, &mut events, result)?;
    let sinks = shutdown(&mut service, workers)?;
    result.shutdown_races += 1;
    recover(&service, sinks, result)
}

fn pressure(
    service: &mut Service<'_>,
    events: &mut [Events<'_>; 2],
    worker: &mut PersistenceWorker<'_, Sink, 16>,
    result: &mut CombinedResult,
) -> Result<(), String> {
    let first = cancel(service, ShardId(0))?;
    admit(service, first)?;
    let second = cancel(service, ShardId(0))?;
    admit(service, second)?;
    let pending = cancel(service, ShardId(0))?;
    let expected = service.expected_sequence();
    let health = service.health(ShardId(0));
    ensure(
        matches!(
            service.process_command(expected, pending, 1),
            Err(hft_engine::SessionAdmissionError::Routed(
                hft_engine::RoutedEngineError::Engine {
                    error: hft_engine::EngineError::Admission(
                        hft_events::EventEngineError::Backpressured
                    ),
                    ..
                }
            ))
        ),
        "event saturation accepted",
    )?;
    ensure(
        service.expected_sequence() == expected && service.health(ShardId(0)) == health,
        "event saturation consumed input",
    )?;
    result.event_pressure += 1;
    pop(&mut events[0], first, result)?;
    pop(&mut events[0], second, result)?;
    admit(service, pending)?;
    pop(&mut events[0], pending, result)?;
    for _ in 3..RING_CAPACITY {
        let command = cancel(service, ShardId(0))?;
        admit(service, command)?;
        pop(&mut events[0], command, result)?;
    }
    let pending = cancel(service, ShardId(0))?;
    let expected = service.expected_sequence();
    let health = service.health(ShardId(0));
    ensure(
        matches!(
            service.process_command(expected, pending, 1),
            Err(hft_engine::SessionAdmissionError::Routed(
                hft_engine::RoutedEngineError::Engine {
                    error: hft_engine::EngineError::Journal(hft_journal::JournalError::Saturated),
                    ..
                }
            ))
        ),
        "journal saturation accepted",
    )?;
    ensure(
        service.expected_sequence() == expected && service.health(ShardId(0)) == health,
        "journal saturation consumed input",
    )?;
    ensure(
        events[0].try_pop().is_none(),
        "journal refusal published event",
    )?;
    result.journal_pressure += 1;
    while checked(worker.drain_batch())? > 0 {}
    admit(service, pending)?;
    pop(&mut events[0], pending, result)?;

    Ok(())
}

fn session_faults(
    service: &mut Service<'_>,
    events: &mut [Events<'_>; 2],
    result: &mut CombinedResult,
) -> Result<(), String> {
    let pending = cancel(service, ShardId(1))?;
    let expected = service.expected_sequence();
    for received in [
        SequenceNumber(expected.0 - 1),
        SequenceNumber(expected.0 + 1),
    ] {
        ensure(
            service.process_command(received, pending, 1).is_err(),
            "session sequence refusal missing",
        )?;
        ensure(
            service.expected_sequence() == expected,
            "session refusal consumed sequence",
        )?;
        result.session_refusals += 1;
    }
    ensure(
        service
            .process_frame(expected, &RxFrame::from_bytes(&[]), 1)
            .is_err(),
        "malformed frame accepted",
    )?;
    result.malformed_frames += 1;
    let Command::CancelOrder(mut unknown) = pending else {
        return Err("unexpected command".into());
    };
    unknown.instrument_id = InstrumentId(99);
    ensure(
        service
            .process_command(expected, Command::CancelOrder(unknown), 1)
            .is_err(),
        "unknown route accepted",
    )?;
    ensure(
        service.expected_sequence() == expected,
        "parse or route error consumed sequence",
    )?;
    result.unknown_routes += 1;
    checked(service.tick(6))?;
    ensure(
        service.state() == SessionState::Recovering,
        "heartbeat did not enter recovery",
    )?;
    result.heartbeat_timeouts += 1;
    admit_at(service, pending, 7)?;
    pop(&mut events[1], pending, result)?;
    let expected = service.expected_sequence();
    checked(service.handle_session(SessionEvent::Disconnect, 8))?;
    ensure(
        service
            .process_command(expected, cancel(service, ShardId(1))?, 8)
            .is_err(),
        "disconnected command accepted",
    )?;
    result.session_refusals += 1;
    activate(service, expected, 9)?;
    result.reconnects += 1;

    Ok(())
}

fn traffic(
    seed: u64,
    service: &mut Service<'_>,
    events: &mut [Events<'_>; 2],
    result: &mut CombinedResult,
) -> Result<(), String> {
    let mut random = seed;
    for _ in 0..32 {
        random = random
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        let index = usize::from(random % 100 >= 85);
        let shard = ShardId(u16::try_from(index).map_err(|_| "shard conversion")?);
        for side in [Side::Buy, Side::Sell] {
            let sequence = service.health(shard).ok_or("shard")?.next_sequence;
            let command = Command::NewOrder(NewOrder {
                order_id: OrderId(sequence.0),
                sequence,
                account_id: AccountId(if side == Side::Buy { 1 } else { 2 }),
                instrument_id: InstrumentId(7 + u32::from(shard.0)),
                side,
                price: PriceTicks(100),
                quantity: Quantity(1),
                time_in_force: TimeInForce::Gtc,
            });
            admit_at(service, command, 10)?;
            pop(&mut events[index], command, result)?;
        }
    }

    Ok(())
}

fn shutdown(
    service: &mut Service<'_>,
    workers: [PersistenceWorker<'_, Sink, 16>; 2],
) -> Result<[Sink; 2], String> {
    // Workers race producer closure. Timing is excluded from the deterministic result.
    let sinks = std::thread::scope(|scope| -> Result<_, String> {
        let (ready, notifications) = std::sync::mpsc::channel();
        let handles = workers.map(|mut worker| {
            let ready = ready.clone();
            scope.spawn(move || -> Result<Sink, String> {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                let observed_open = checked(worker.drain_batch()).and_then(|_| {
                    ensure(
                        matches!(
                            worker.shutdown(),
                            Err(hft_journal::PersistError::ProducerOpen)
                        ),
                        "worker did not observe open producer",
                    )
                });
                ready
                    .send(observed_open.clone())
                    .map_err(|_| "shutdown readiness channel closed")?;
                observed_open?;
                loop {
                    checked(worker.drain_batch())?;
                    match worker.shutdown() {
                        Ok(()) => return checked(worker.into_sink()),
                        Err(hft_journal::PersistError::ProducerOpen) => {
                            ensure(
                                std::time::Instant::now() < deadline,
                                "combined shutdown deadline",
                            )?;
                            std::thread::yield_now();
                        }
                        Err(error) => return Err(format!("combined shutdown: {error:?}")),
                    }
                }
            })
        });
        drop(ready);
        let readiness = (0..2).try_for_each(|_| {
            notifications
                .recv_timeout(std::time::Duration::from_secs(30))
                .map_err(|error| format!("shutdown readiness: {error}"))?
        });
        // Close even if readiness failed so a surviving worker can terminate.
        service.stop_admission();
        let [a, b] = handles;
        let a = a
            .join()
            .map_err(|_| "combined persistence thread panicked")?;
        let b = b
            .join()
            .map_err(|_| "combined persistence thread panicked")?;
        readiness?;
        Ok([a?, b?])
    })?;
    Ok(sinks)
}

fn recover(
    service: &Service<'_>,
    sinks: [Sink; 2],
    result: &mut CombinedResult,
) -> Result<(), String> {
    for (index, sink) in sinks.into_iter().enumerate() {
        let shard = ShardId(u16::try_from(index).map_err(|_| "shard conversion")?);
        let instrument = InstrumentId(7 + u32::from(shard.0));
        let health = service.health(shard).ok_or("shard")?;
        ensure(
            health.state == EngineState::Stopped,
            "combined engine not stopped",
        )?;
        ensure(
            sink.bytes.len() as u64 == (health.next_sequence.0 - 1) * RECORD_SIZE as u64,
            "journal lost or duplicated input",
        )?;
        let snapshot = checked(service.snapshot(shard))?;
        let mut risk = RiskEngine::<2, 16>::new();
        for account in [AccountId(1), AccountId(2)] {
            checked(risk.register_account(account, limits()))?;
        }
        let initial = checked(encode_snapshot(
            &Gateway::<2, 16, 4, 4>::new(risk, instrument),
            0,
        ))?;
        let expected_config = builder(instrument)?.configuration().clone();
        let persisted_config = expected_config.encode();
        let restored = checked(Builder::restore_configured(
            &expected_config,
            &persisted_config,
            initial.bytes(),
            &sink.bytes,
        ))?;
        let mut fresh = checked(EngineStorage::<6, 2>::try_new())?;
        let EngineParts {
            mut engine,
            mut events,
            journal,
        } = checked(restored.build(&mut fresh))?;
        let mut worker = checked(PersistenceWorker::<_, 2>::new(
            journal,
            Sink::default(),
            FlushPolicy::OnShutdown,
        ))?;
        engine.stop_admission();
        checked(worker.shutdown())?;
        ensure(
            checked(engine.snapshot())?.bytes() == snapshot.bytes(),
            "combined replay diverged",
        )?;
        result.fingerprint = result.fingerprint.rotate_left(7)
            ^ u64::from_be_bytes(
                snapshot.digest()[..8]
                    .try_into()
                    .map_err(|_| "snapshot digest")?,
            );
        ensure(
            events.try_pop().is_none(),
            "restore republished historical events",
        )?;
        resume_checkpoint(&expected_config, &snapshot, health.next_sequence, result)?;
        result.recovery_checks += 1;
    }
    Ok(())
}

// A second restart resumes from the authoritative checkpoint, with fresh queues.
fn resume_checkpoint(
    configuration: &hft_engine::EngineConfig,
    snapshot: &hft_recovery::Snapshot,
    next_sequence: SequenceNumber,
    result: &mut CombinedResult,
) -> Result<(), String> {
    let resumed = checked(Builder::restore_configured(
        configuration,
        &configuration.encode(),
        snapshot.bytes(),
        &[],
    ))?;
    let mut resumed_storage = checked(EngineStorage::<6, 2>::try_new())?;
    let EngineParts {
        mut engine,
        mut events,
        journal,
    } = checked(resumed.build(&mut resumed_storage))?;
    ensure(
        events.try_pop().is_none(),
        "checkpoint restore republished events",
    )?;
    ensure(
        engine.expected_sequence() == next_sequence,
        "restart sequence changed",
    )?;
    let mut resumed_worker = checked(PersistenceWorker::<_, 2>::new(
        journal,
        Sink::default(),
        FlushPolicy::OnShutdown,
    ))?;
    let next_command = Command::CancelOrder(CancelOrder {
        instrument_id: configuration.instrument,
        sequence: next_sequence,
        order_id: OrderId(u64::MAX),
        account_id: AccountId(1),
    });
    checked(engine.process_command(next_command))?;
    pop(&mut events, next_command, result)?;
    ensure(
        events.try_pop().is_none(),
        "resumed command published multiple batches",
    )?;
    ensure(
        engine.expected_sequence().0 == next_sequence.0 + 1,
        "resumed command did not consume exactly one sequence",
    )?;
    engine.stop_admission();
    checked(resumed_worker.shutdown())?;
    let resumed_sink = checked(resumed_worker.into_sink())?;
    ensure(
        resumed_sink.bytes.len() == RECORD_SIZE,
        "resumed journal did not contain exactly one command",
    )?;
    let oracle = checked(hft_recovery::recover_snapshot_and_tail::<2, 16, 4, 4, 4>(
        snapshot.bytes(),
        &resumed_sink.bytes,
    ))?;
    let oracle_snapshot = checked(encode_snapshot(&oracle, next_sequence.0))?;
    let resumed_snapshot = checked(engine.snapshot())?;
    ensure(
        resumed_snapshot.bytes() == oracle_snapshot.bytes(),
        "resumed checkpoint and contiguous replay diverged",
    )?;
    result.fingerprint = result.fingerprint.rotate_left(7)
        ^ u64::from_be_bytes(
            resumed_snapshot.digest()[..8]
                .try_into()
                .map_err(|_| "resumed snapshot digest")?,
        );
    Ok(())
}

fn run_failure(mode: u8, result: &mut CombinedResult) -> Result<(), String> {
    let mut storage0 = checked(EngineStorage::<6, 2>::try_new())?;
    let mut storage1 = checked(EngineStorage::<6, 2>::try_new())?;
    let EngineParts {
        engine: engine0,
        events: mut events0,
        journal: journal0,
    } = checked(builder(InstrumentId(7))?.build(&mut storage0))?;
    let EngineParts {
        engine: engine1,
        events: _events1,
        journal: journal1,
    } = checked(builder(InstrumentId(8))?.build(&mut storage1))?;
    let mut service = service([engine0, engine1])?;
    let mut worker = checked(PersistenceWorker::<_, 2>::new(
        journal0,
        Sink {
            fail_write: mode == 0,
            fail_flush: mode == 1,
            ..Sink::default()
        },
        FlushPolicy::EveryBatch,
    ))?;
    let _worker1 = checked(PersistenceWorker::<_, 2>::new(
        journal1,
        Sink::default(),
        FlushPolicy::OnShutdown,
    ))?;
    let command = cancel(&service, ShardId(0))?;
    admit(&mut service, command)?;
    pop(&mut events0, command, result)?;
    if mode == 2 {
        drop(worker);
        result.abandoned_workers += 1;
    } else {
        ensure(
            worker.drain_batch().is_err(),
            "injected persistence failure was missed",
        )?;
        if mode == 0 {
            result.write_failures += 1;
        } else {
            result.flush_failures += 1;
        }
    }
    let expected = service.expected_sequence();
    ensure(
        service
            .process_command(expected, cancel(&service, ShardId(1))?, 1)
            .is_err(),
        "failed service accepted input",
    )?;
    ensure(
        service.state() == SessionState::Failed,
        "persistence failure left session open",
    )?;
    ensure(
        events0.try_pop().is_none(),
        "failed service published input",
    )?;
    ensure(
        service.snapshot(ShardId(0)).is_err(),
        "failed service produced checkpoint",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn combined_faults_repeat_and_cover_all_boundaries() {
        if hft_spsc::IS_LOOM_BUILD {
            return;
        }
        let first = run_combined(Seed(1), 2).expect("combined run");
        assert_eq!(first, run_combined(Seed(1), 2).expect("repeat"));
        assert_eq!(first.event_pressure, 2);
        assert_eq!(first.journal_pressure, 2);
        assert_eq!(first.recovery_checks, 4);
        assert_eq!(first.shutdown_races, 2);
        assert_eq!(first.write_failures, 2);
        assert_eq!(first.flush_failures, 2);
        assert_eq!(first.abandoned_workers, 2);
        assert!(first.commands > 2_000);
    }
}
