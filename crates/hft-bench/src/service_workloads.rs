//! Two instrument admission through the routed and session engine boundaries.
//! Event consumption and fixed-memory journal persistence are untimed but gated.

use crate::record::{BenchRecord, Extra};
use crate::{allocation_gate, assert_allocation_gate, push_latency_record};
use hft_engine::{
    EngineBuilder, EngineParts, EngineState, EngineStorage, Health, InstrumentRoute, RouteTable,
    RoutedEngine, SessionConfig, SessionEngine, SessionEvent, SessionState, ShardId,
};
use hft_events::{Event, EventBatch};
use hft_io::RxFrame;
use hft_journal::{DurableSink, FlushPolicy, JournalRecord, PersistenceWorker, RECORD_SIZE};
use hft_risk::RiskLimits;
use hft_spsc::Consumer;
use hft_types::{
    AccountId, CancelOrder, Command, InstrumentId, NewOrder, OrderId, OrderState, PriceTicks,
    Quantity, SequenceNumber, Side, TimeInForce,
};
use hft_wire::{encode_cancel_order, encode_new_order};
use std::{hint::black_box, io, time::Instant};

const WARMUP: usize = 128;
const INSTRUMENTS: [InstrumentId; 2] = [InstrumentId(7), InstrumentId(19)];
type Routed<'a> = RoutedEngine<'a, 2, 1, 8, 2, 4, 2, 4, 2>;
type Service<'a> = SessionEngine<'a, 2, 1, 8, 2, 4, 2, 4, 2>;

struct FixedSink {
    instrument: InstrumentId,
    next_sequence: u64,
    checksum: u64,
    last_record: [u8; RECORD_SIZE],
    flushes: u64,
}

impl FixedSink {
    const fn new(instrument: InstrumentId) -> Self {
        Self {
            instrument,
            next_sequence: 1,
            checksum: 0,
            last_record: [0; RECORD_SIZE],
            flushes: 0,
        }
    }
}

impl DurableSink for FixedSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        assert_eq!(bytes.len(), RECORD_SIZE);
        self.last_record.copy_from_slice(black_box(bytes));
        let record = JournalRecord::decode(&self.last_record).expect("valid persisted record");
        assert_eq!(record.sequence(), SequenceNumber(self.next_sequence));
        let (expected, len) = wire(command(self.instrument, self.next_sequence));
        assert_eq!(record.slice().expect("payload"), &expected[..len]);
        self.checksum = fold(self.checksum, bytes);
        self.next_sequence += 1;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

fn fold(mut checksum: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        checksum = checksum.rotate_left(7) ^ u64::from(*byte);
    }
    checksum
}

fn command(instrument: InstrumentId, sequence: u64) -> Command {
    if sequence % 2 == 1 {
        Command::NewOrder(NewOrder {
            order_id: OrderId(sequence),
            account_id: AccountId(1),
            instrument_id: instrument,
            sequence: SequenceNumber(sequence),
            price: PriceTicks(100),
            quantity: Quantity(2),
            side: Side::Buy,
            time_in_force: TimeInForce::Gtc,
        })
    } else {
        Command::CancelOrder(CancelOrder {
            order_id: OrderId(sequence - 1),
            account_id: AccountId(1),
            instrument_id: instrument,
            sequence: SequenceNumber(sequence),
        })
    }
}

fn wire(command: Command) -> ([u8; 46], usize) {
    match command {
        Command::NewOrder(order) => (encode_new_order(order), 46),
        Command::CancelOrder(cancel) => {
            let encoded = encode_cancel_order(cancel);
            let mut bytes = [0; 46];
            bytes[..encoded.len()].copy_from_slice(&encoded);
            (bytes, encoded.len())
        }
        Command::ReplaceOrder(_) => unreachable!("fixture has no replacements"),
    }
}

// Static dispatch keeps benchmark adapters out of the measured boundary.
trait Boundary {
    fn apply<const FRAME: bool>(
        &mut self,
        transport: SequenceNumber,
        command: Command,
        bytes: &[u8],
    ) -> Result<ShardId, ()>;
    fn verify_transport(&self, _: SequenceNumber) {}
    fn health(&self, shard: ShardId) -> Health;
    fn stop(&mut self);
    fn digest(&self, shard: ShardId) -> [u8; 32];
}

impl Boundary for Routed<'_> {
    fn apply<const FRAME: bool>(
        &mut self,
        _: SequenceNumber,
        command: Command,
        bytes: &[u8],
    ) -> Result<ShardId, ()> {
        let result = if FRAME {
            self.process_frame(&RxFrame::from_bytes(bytes))
        } else {
            self.process_command(command)
        };
        result.map_err(|_| ())
    }
    fn health(&self, shard: ShardId) -> Health {
        self.health(shard).expect("shard")
    }
    fn stop(&mut self) {
        self.stop_admission();
    }
    fn digest(&self, shard: ShardId) -> [u8; 32] {
        self.snapshot(shard).expect("snapshot").digest()
    }
}

impl Boundary for Service<'_> {
    fn apply<const FRAME: bool>(
        &mut self,
        transport: SequenceNumber,
        command: Command,
        bytes: &[u8],
    ) -> Result<ShardId, ()> {
        let result = if FRAME {
            self.process_frame(transport, &RxFrame::from_bytes(bytes), transport.0)
        } else {
            self.process_command(transport, command, transport.0)
        };
        result.map_err(|_| ())
    }
    fn verify_transport(&self, transport: SequenceNumber) {
        assert_eq!(self.expected_sequence(), SequenceNumber(transport.0 + 1));
        assert_eq!(self.state(), SessionState::Active);
    }
    fn health(&self, shard: ShardId) -> Health {
        self.health(shard).expect("shard")
    }
    fn stop(&mut self) {
        self.stop_admission();
    }
    fn digest(&self, shard: ShardId) -> [u8; 32] {
        self.snapshot(shard).expect("snapshot").digest()
    }
}

fn expected_shard(command: Command) -> ShardId {
    ShardId(u16::from(command.instrument_id() != INSTRUMENTS[0]))
}

/// # Panics
/// Panics on fixture failure, allocation, or differing boundary results.
pub fn service_benchmarks(samples: usize, out: &mut Vec<BenchRecord>) {
    if samples == 0 {
        return;
    }
    let reference = cell::<false, false>(samples, out);
    assert_eq!(reference, cell::<false, true>(samples, out));
    assert_eq!(reference, cell::<true, false>(samples, out));
    assert_eq!(reference, cell::<true, true>(samples, out));
}

// Keep the fixed queue, worker and facade initialization together for review.
#[allow(clippy::too_many_lines)]
fn cell<const SESSION: bool, const FRAME: bool>(
    samples: usize,
    out: &mut Vec<BenchRecord>,
) -> (u64, [[u8; 32]; 2]) {
    let mut zero = EngineStorage::<4, 2>::try_new().expect("storage");
    let mut one = EngineStorage::<4, 2>::try_new().expect("storage");
    let build = |instrument| {
        EngineBuilder::<1, 8, 2, 4, 2>::new(
            instrument,
            &[(
                AccountId(1),
                RiskLimits {
                    max_quantity: Quantity(10),
                    max_notional: 10_000,
                    max_abs_position: Quantity(1_000),
                    max_open_orders: 8,
                    minimum_price: PriceTicks(1),
                    maximum_price: PriceTicks(1_000),
                },
            )],
        )
        .expect("builder")
    };
    let EngineParts {
        engine: zero_engine,
        events: zero_events,
        journal: zero_journal,
    } = build(INSTRUMENTS[0]).build(&mut zero).expect("engine");
    let EngineParts {
        engine: one_engine,
        events: one_events,
        journal: one_journal,
    } = build(INSTRUMENTS[1]).build(&mut one).expect("engine");
    let routes = RouteTable::try_new([
        InstrumentRoute {
            instrument_id: INSTRUMENTS[0],
            shard_id: ShardId(0),
        },
        InstrumentRoute {
            instrument_id: INSTRUMENTS[1],
            shard_id: ShardId(1),
        },
    ])
    .expect("routes");
    let boundary = RoutedEngine::try_new(routes, [zero_engine, one_engine]).expect("routed");
    let mut consumers = [zero_events, one_events];
    let mut workers = [
        PersistenceWorker::<_, 1>::new(
            zero_journal,
            FixedSink::new(INSTRUMENTS[0]),
            FlushPolicy::OnShutdown,
        )
        .expect("worker"),
        PersistenceWorker::<_, 1>::new(
            one_journal,
            FixedSink::new(INSTRUMENTS[1]),
            FlushPolicy::OnShutdown,
        )
        .expect("worker"),
    ];
    let mut timings = vec![0; samples];
    let (checksum, digests) = if SESSION {
        let mut service = SessionEngine::new(
            SessionConfig {
                logon_timeout_ticks: 0,
                heartbeat_timeout_ticks: 0,
            },
            boundary,
        );
        for event in [
            SessionEvent::Connect,
            SessionEvent::LogonSent,
            SessionEvent::LogonAccepted {
                first_sequence: SequenceNumber(1),
            },
        ] {
            service.handle_session(event, 0).expect("activate");
        }
        sample::<_, FRAME>(&mut service, &mut consumers, &mut workers, &mut timings)
    } else {
        sample::<_, FRAME>(
            &mut { boundary },
            &mut consumers,
            &mut workers,
            &mut timings,
        )
    };
    let sinks = workers.map(|worker| worker.into_sink().expect("clean sink"));
    let mut combined = checksum;
    for (shard, sink) in sinks.iter().enumerate() {
        assert_eq!(
            sink.next_sequence - 1,
            ((WARMUP + samples + 1 - shard) / 2) as u64
        );
        assert_eq!(sink.flushes, 1);
        combined ^= sink.checksum.rotate_left(if shard == 0 { 13 } else { 29 });
    }
    push_latency_record(
        out,
        BenchRecord {
            checksum: combined,
            ..BenchRecord::new(
                "gateway",
                "engine",
                "routed_journaled_admission",
                &[
                    (
                        "path",
                        Extra::Text(if SESSION { "session" } else { "routed" }),
                    ),
                    (
                        "input",
                        Extra::Text(if FRAME { "frame" } else { "command" }),
                    ),
                    ("shards", Extra::U64(2)),
                ],
            )
        },
        &mut timings,
    );
    (combined, digests)
}

fn sample<E: Boundary, const FRAME: bool>(
    engine: &mut E,
    consumers: &mut [Consumer<'_, EventBatch<4>, 2>; 2],
    workers: &mut [PersistenceWorker<'_, FixedSink, 1>; 2],
    timings: &mut [u64],
) -> (u64, [[u8; 32]; 2]) {
    let mut checksum = 0_u64;
    let gate = allocation_gate();
    for step in 0..WARMUP + timings.len() {
        let shard = step % 2;
        let sequence = (step / 2 + 1) as u64;
        let command = command(INSTRUMENTS[shard], sequence);
        let (bytes, len) = wire(command);
        let started = Instant::now();
        let result = black_box(&mut *engine).apply::<FRAME>(
            SequenceNumber(step as u64 + 1),
            black_box(command),
            black_box(&bytes[..len]),
        );
        let elapsed = started.elapsed().as_nanos();
        assert_eq!(
            black_box(result).expect("admission"),
            expected_shard(command)
        );
        engine.verify_transport(SequenceNumber(step as u64 + 1));
        assert_eq!(
            engine
                .health(ShardId(u16::try_from(shard).expect("two shards")))
                .next_sequence,
            SequenceNumber(sequence + 1)
        );
        let batch = consumers[shard].try_pop().expect("event batch");
        assert_eq!(batch.len(), 2);
        let mut events = batch.iter();
        let order_id = if sequence % 2 == 1 {
            sequence
        } else {
            sequence - 1
        };
        match events.next().expect("outcome") {
            Event::Accepted(event) => {
                assert_eq!(sequence % 2, 1);
                assert_eq!(event.state, OrderState::Accepted);
                assert_eq!(event.resting_quantity, Quantity(2));
                assert_eq!(event.filled_quantity, Quantity(0));
                assert_eq!(event.discarded_quantity, Quantity(0));
                assert_eq!(event.order_id, OrderId(order_id));
                assert_eq!(event.account_id, AccountId(1));
                assert_eq!(event.instrument_id, INSTRUMENTS[shard]);
                assert_eq!(event.id.command_sequence, SequenceNumber(sequence));
            }
            Event::Cancelled(event) => {
                assert_eq!(sequence % 2, 0);
                assert_eq!(event.order_id, OrderId(order_id));
                assert_eq!(event.quantity, Quantity(2));
                assert_eq!(event.instrument_id, INSTRUMENTS[shard]);
                assert_eq!(event.id.command_sequence, SequenceNumber(sequence));
            }
            event => panic!("unexpected outcome: {event:?}"),
        }
        let Event::TopOfBook(top) = events.next().expect("top") else {
            panic!("expected top");
        };
        assert_eq!(top.id.command_sequence, SequenceNumber(sequence));
        assert_eq!(top.instrument_id, INSTRUMENTS[shard]);
        assert!(top.ask.is_none());
        assert_eq!(top.bid.is_some(), sequence % 2 == 1);
        assert!(consumers[shard].try_pop().is_none());
        assert_eq!(workers[shard].drain_batch().expect("persist"), 1);
        if step >= WARMUP {
            timings[step - WARMUP] = u64::try_from(elapsed).unwrap_or(u64::MAX);
            checksum = checksum.rotate_left(7)
                ^ sequence
                ^ u64::from(INSTRUMENTS[shard].0)
                ^ order_id.rotate_left(17);
        }
    }
    engine.stop();
    for worker in workers {
        worker.shutdown().expect("flush");
    }
    assert_allocation_gate(gate, "routed service admission, events and persistence");
    for shard in [ShardId(0), ShardId(1)] {
        assert_eq!(engine.health(shard).state, EngineState::Stopped);
    }
    (
        checksum,
        [engine.digest(ShardId(0)), engine.digest(ShardId(1))],
    )
}
