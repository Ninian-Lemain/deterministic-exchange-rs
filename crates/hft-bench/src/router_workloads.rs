use crate::record::{BenchRecord, Extra};
use crate::{ALLOCATIONS, DEALLOCATIONS, push_latency_record};
use hft_events::EventBatch;
use hft_gateway::Gateway;
use hft_risk::{RiskEngine, RiskLimits};
use hft_router::{
    InstrumentRoute, MatchingShard, MultiInstrumentRouter, RouteTable, RouterError, ShardId,
    ShardStep,
};
use hft_spsc::SpscQueue;
use hft_types::{
    AccountId, Command, InstrumentId, NewOrder, OrderId, PriceTicks, Quantity, SequenceNumber,
    Side, TimeInForce,
};
use std::sync::atomic::Ordering;
use std::time::Instant;

const FIRST: InstrumentRoute = InstrumentRoute {
    instrument_id: InstrumentId(11),
    shard_id: ShardId(0),
};
const SECOND: InstrumentRoute = InstrumentRoute {
    instrument_id: InstrumentId(22),
    shard_id: ShardId(1),
};
const QUEUE_CAPACITY: usize = 64;
const BATCH_CAPACITY: usize = 3;
const WARMUP: usize = 128;

type BenchBatch = EventBatch<BATCH_CAPACITY>;
type BenchGateway = Gateway<2, 16, 4, 4>;

pub fn router_benchmarks(samples: usize, out: &mut Vec<BenchRecord>) {
    route_command(samples, out);
    route_shard_event(samples, out);
    full_command_queue(samples, out);
}

fn gateway(instrument_id: InstrumentId) -> BenchGateway {
    let limits = RiskLimits {
        max_quantity: Quantity(10),
        max_notional: 10_000,
        max_abs_position: Quantity(10_000),
        max_open_orders: 16,
        minimum_price: PriceTicks(1),
        maximum_price: PriceTicks(1_000),
    };
    let mut risk = RiskEngine::new();
    risk.register_account(AccountId(1), limits)
        .expect("router benchmark account");
    risk.register_account(AccountId(2), limits)
        .expect("router benchmark peer");
    Gateway::new(risk, instrument_id)
}

const fn command(order_id: u64, instrument_id: InstrumentId, sequence: u64) -> Command {
    Command::NewOrder(NewOrder {
        order_id: OrderId(order_id),
        account_id: AccountId(1),
        instrument_id,
        price: PriceTicks(100),
        quantity: Quantity(1),
        sequence: SequenceNumber(sequence),
        side: Side::Buy,
        time_in_force: TimeInForce::Ioc,
    })
}

fn route_command(samples: usize, out: &mut Vec<BenchRecord>) {
    let mut command_zero = SpscQueue::<Command, QUEUE_CAPACITY>::try_new().expect("command zero");
    let mut command_one = SpscQueue::<Command, QUEUE_CAPACITY>::try_new().expect("command one");
    let (command_zero_producer, mut command_zero_consumer) = command_zero.split();
    let (command_one_producer, mut command_one_consumer) = command_one.split();
    let mut event_zero = SpscQueue::<BenchBatch, 1>::try_new().expect("event zero");
    let mut event_one = SpscQueue::<BenchBatch, 1>::try_new().expect("event one");
    let (_, event_zero_consumer) = event_zero.split();
    let (_, event_one_consumer) = event_one.split();
    let mut router = MultiInstrumentRouter::new(
        RouteTable::try_new([SECOND, FIRST]).expect("router table"),
        [command_zero_producer, command_one_producer],
        [event_zero_consumer, event_one_consumer],
    );

    for iteration in 0..WARMUP {
        let instrument_id = if iteration & 1 == 0 {
            FIRST.instrument_id
        } else {
            SECOND.instrument_id
        };
        let sequence = u64::try_from(iteration).unwrap_or(u64::MAX) + 1;
        let shard_id = router
            .route_command(command(sequence, instrument_id, sequence))
            .expect("warmup route");
        let received = if shard_id == FIRST.shard_id {
            command_zero_consumer.try_pop()
        } else {
            command_one_consumer.try_pop()
        };
        assert!(received.is_some());
    }

    let sample_count = samples.min(2_000);
    let mut timings = [0_u64; 2_000];
    let allocations_before = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_before = DEALLOCATIONS.load(Ordering::SeqCst);
    let mut checksum = 0_u64;
    for (index, sample) in timings[..sample_count].iter_mut().enumerate() {
        let instrument_id = if index & 1 == 0 {
            FIRST.instrument_id
        } else {
            SECOND.instrument_id
        };
        let sequence = u64::try_from(WARMUP + index).unwrap_or(u64::MAX) + 1;
        let command_to_route = command(sequence, instrument_id, sequence);
        let started = Instant::now();
        let shard_id = router
            .route_command(command_to_route)
            .expect("measured route");
        *sample = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let received = if shard_id == FIRST.shard_id {
            command_zero_consumer.try_pop()
        } else {
            command_one_consumer.try_pop()
        };
        assert_eq!(received, Some(command_to_route));
        checksum ^= sequence.rotate_left(u32::from(shard_id.0));
    }

    push_latency_record(
        out,
        BenchRecord {
            allocations: ALLOCATIONS
                .load(Ordering::SeqCst)
                .saturating_sub(allocations_before),
            deallocations: DEALLOCATIONS
                .load(Ordering::SeqCst)
                .saturating_sub(deallocations_before),
            checksum,
            ..BenchRecord::new(
                "gateway",
                "router",
                "route_command",
                &[
                    ("shards", Extra::U64(2)),
                    ("instruments", Extra::U64(2)),
                    (
                        "queue_capacity",
                        Extra::U64(u64::try_from(QUEUE_CAPACITY).unwrap_or(u64::MAX)),
                    ),
                ],
            )
        },
        &mut timings[..sample_count],
    );
}

fn route_shard_event(samples: usize, out: &mut Vec<BenchRecord>) {
    let mut command_zero = SpscQueue::<Command, QUEUE_CAPACITY>::try_new().expect("command zero");
    let mut command_one = SpscQueue::<Command, QUEUE_CAPACITY>::try_new().expect("command one");
    let (command_zero_producer, command_zero_consumer) = command_zero.split();
    let (command_one_producer, command_one_consumer) = command_one.split();
    let mut event_zero = SpscQueue::<BenchBatch, QUEUE_CAPACITY>::try_new().expect("event zero");
    let mut event_one = SpscQueue::<BenchBatch, QUEUE_CAPACITY>::try_new().expect("event one");
    let (event_zero_producer, event_zero_consumer) = event_zero.split();
    let (event_one_producer, event_one_consumer) = event_one.split();
    let mut router = MultiInstrumentRouter::new(
        RouteTable::try_new([FIRST, SECOND]).expect("router table"),
        [command_zero_producer, command_one_producer],
        [event_zero_consumer, event_one_consumer],
    );
    let mut shard_zero = MatchingShard::<2, 16, 4, 4, 1, 3, 64, 64>::try_new(
        FIRST,
        gateway(FIRST.instrument_id),
        command_zero_consumer,
        event_zero_producer,
    )
    .expect("shard zero");
    let mut shard_one = MatchingShard::<2, 16, 4, 4, 1, 3, 64, 64>::try_new(
        SECOND,
        gateway(SECOND.instrument_id),
        command_one_consumer,
        event_one_producer,
    )
    .expect("shard one");
    let sample_count = samples.min(2_000);
    let mut timings = [0_u64; 2_000];
    let allocations_before;
    let deallocations_before;
    let mut checksum = 0_u64;
    {
        let mut next_sequence = [1_u64; 2];
        let mut run_one = |iteration: usize| {
            let shard_index = iteration & 1;
            let route = if shard_index == 0 { FIRST } else { SECOND };
            let sequence = next_sequence[shard_index];
            next_sequence[shard_index] += 1;
            let command_to_route = command(sequence, route.instrument_id, sequence);
            let shard_id = router
                .route_command(command_to_route)
                .expect("route command");
            let step = if shard_id == FIRST.shard_id {
                shard_zero.try_process_one()
            } else {
                shard_one.try_process_one()
            }
            .expect("shard command");
            assert_eq!(step, ShardStep::Processed(SequenceNumber(sequence)));
            let batch = router
                .try_event(shard_id)
                .expect("known shard")
                .expect("event batch");
            u64::try_from(batch.len()).unwrap_or(u64::MAX)
                ^ sequence.rotate_left(u32::from(shard_id.0))
        };

        for iteration in 0..WARMUP {
            let _ = run_one(iteration);
        }
        allocations_before = ALLOCATIONS.load(Ordering::SeqCst);
        deallocations_before = DEALLOCATIONS.load(Ordering::SeqCst);
        for (index, sample) in timings[..sample_count].iter_mut().enumerate() {
            let started = Instant::now();
            checksum ^= run_one(WARMUP + index);
            *sample = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        }
    }
    checksum ^= shard_zero.gateway().stable_digest();
    checksum ^= shard_one.gateway().stable_digest().rotate_left(1);

    push_latency_record(
        out,
        BenchRecord {
            allocations: ALLOCATIONS
                .load(Ordering::SeqCst)
                .saturating_sub(allocations_before),
            deallocations: DEALLOCATIONS
                .load(Ordering::SeqCst)
                .saturating_sub(deallocations_before),
            checksum,
            ..BenchRecord::new(
                "gateway",
                "router",
                "route_shard_event",
                &[
                    ("shards", Extra::U64(2)),
                    ("instruments", Extra::U64(2)),
                    (
                        "queue_capacity",
                        Extra::U64(u64::try_from(QUEUE_CAPACITY).unwrap_or(u64::MAX)),
                    ),
                    (
                        "event_batch_capacity",
                        Extra::U64(u64::try_from(BATCH_CAPACITY).unwrap_or(u64::MAX)),
                    ),
                ],
            )
        },
        &mut timings[..sample_count],
    );
}

fn full_command_queue(samples: usize, out: &mut Vec<BenchRecord>) {
    let mut commands = SpscQueue::<Command, 1>::try_new().expect("command queue");
    let (command_producer, _command_consumer) = commands.split();
    let mut events = SpscQueue::<BenchBatch, 1>::try_new().expect("event queue");
    let (_, event_consumer) = events.split();
    let mut router = MultiInstrumentRouter::new(
        RouteTable::try_new([FIRST]).expect("router table"),
        [command_producer],
        [event_consumer],
    );
    router
        .route_command(command(1, FIRST.instrument_id, 1))
        .expect("fill command queue");
    let refused = command(2, FIRST.instrument_id, 2);
    for _ in 0..WARMUP {
        assert_eq!(
            router.route_command(refused),
            Err(RouterError::CommandBackpressured(ShardId(0)))
        );
    }

    let sample_count = samples.min(2_000);
    let mut timings = [0_u64; 2_000];
    let allocations_before = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_before = DEALLOCATIONS.load(Ordering::SeqCst);
    let mut backpressure_count = 0_u64;
    for sample in &mut timings[..sample_count] {
        let started = Instant::now();
        let result = router.route_command(refused);
        *sample = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        if result == Err(RouterError::CommandBackpressured(ShardId(0))) {
            backpressure_count += 1;
        }
    }

    push_latency_record(
        out,
        BenchRecord {
            allocations: ALLOCATIONS
                .load(Ordering::SeqCst)
                .saturating_sub(allocations_before),
            deallocations: DEALLOCATIONS
                .load(Ordering::SeqCst)
                .saturating_sub(deallocations_before),
            checksum: backpressure_count,
            ..BenchRecord::new(
                "gateway",
                "router",
                "full_command_queue",
                &[
                    ("shards", Extra::U64(1)),
                    ("instruments", Extra::U64(1)),
                    ("queue_capacity", Extra::U64(1)),
                    ("backpressure_count", Extra::U64(backpressure_count)),
                ],
            )
        },
        &mut timings[..sample_count],
    );
}
