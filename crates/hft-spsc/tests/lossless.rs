#![cfg(not(feature = "loom"))]
//! Losslessness properties: accepted elements appear exactly once in FIFO
//! order, and rejected pushes hand the value back intact.

use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};

use hft_spsc::{Consumer, SpscQueue};

/// Deterministic `SplitMix64` so the schedules are reproducible with zero
/// dependencies. Bounded draws use multiply-shift, no division.
struct SplitMix64(u64);

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_97F4_A615);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        (self.next_u64() >> 32).wrapping_mul(bound) >> 32
    }
}

const SEEDS: [u64; 4] = [
    0x0000_0000_0000_0001,
    0x0123_4567_89AB_CDEF,
    0xDEAD_BEEF_CAFE_F00D,
    0x9E37_79B9_97F4_A615,
];

/// Iteration budget shrinks under Miri to keep qualification fast.
const STEPS_PER_SEED: u64 = if cfg!(miri) { 64 } else { 2_048 };

/// One pop checked against the shadow deque. Returns whether a value came
/// out.
fn pop_matches_shadow<const N: usize>(
    consumer: &mut Consumer<'_, u64, N>,
    shadow: &mut VecDeque<u64>,
    context: &str,
) -> bool {
    if let Some(value) = consumer.try_pop() {
        let expected = shadow
            .pop_front()
            .unwrap_or_else(|| panic!("{context}: queue yielded {value} while shadow was empty"));
        assert_eq!(value, expected, "{context}");
        true
    } else {
        assert!(
            shadow.is_empty(),
            "{context}: pop returned None with {} items pending",
            shadow.len()
        );
        false
    }
}

/// Drives a seeded interleaved schedule of push/pop/idle attempts against a
/// shadow `VecDeque`, asserting the invariants after every operation, then
/// drains until `None`.
fn run_lossless_schedule<const N: usize>(seed: u64, steps: u64) {
    let mut rng = SplitMix64::new(seed);
    let mut queue = SpscQueue::<u64, N>::try_new().expect("valid capacity");
    assert_eq!(queue.capacity(), N);
    let (mut producer, mut consumer) = queue.split();
    let mut shadow: VecDeque<u64> = VecDeque::with_capacity(N);

    for step in 0..steps {
        let context = format!("seed {seed} capacity {N} step {step}");
        match rng.below(4) {
            0 | 1 => {
                let tag = rng.below(0x100);
                let value = (step << 8) | tag;
                let pending_before = shadow.len();
                match producer.try_push(value) {
                    Ok(()) => shadow.push_back(value),
                    Err(returned) => {
                        assert_eq!(
                            returned, value,
                            "{context}: rejected push corrupted payload"
                        );
                        assert_eq!(returned >> 8, step, "{context}: step checksum mismatch");
                        assert_eq!(returned & 0xFF, tag, "{context}: tag checksum mismatch");
                        assert_eq!(shadow.len(), pending_before, "{context}: shadow changed");
                    }
                }
            }
            2 => {
                pop_matches_shadow::<N>(&mut consumer, &mut shadow, &context);
            }
            _ => {}
        }
    }

    while pop_matches_shadow::<N>(&mut consumer, &mut shadow, "final drain") {}
    assert!(shadow.is_empty(), "final drain left shadow non-empty");
}

#[test]
fn seeded_interleaved_schedule_is_lossless_at_capacity_1() {
    for seed in SEEDS {
        run_lossless_schedule::<1>(seed, STEPS_PER_SEED);
    }
}

#[test]
fn seeded_interleaved_schedule_is_lossless_at_capacity_2() {
    for seed in SEEDS {
        run_lossless_schedule::<2>(seed, STEPS_PER_SEED);
    }
}

#[test]
fn seeded_interleaved_schedule_is_lossless_at_capacity_4() {
    for seed in SEEDS {
        run_lossless_schedule::<4>(seed, STEPS_PER_SEED);
    }
}

#[test]
fn seeded_interleaved_schedule_is_lossless_at_capacity_64() {
    for seed in SEEDS {
        run_lossless_schedule::<64>(seed, STEPS_PER_SEED);
    }
}

#[test]
fn wraparound_across_ring_boundary_preserves_order() {
    const CAPACITY: usize = 4;
    let mut queue = SpscQueue::<u64, CAPACITY>::try_new().expect("valid capacity");
    let (mut producer, mut consumer) = queue.split();
    let mut shadow: VecDeque<u64> = VecDeque::new();
    let mut next_value = 0_u64;

    for lap in 0..2 {
        // Fill to capacity.
        for _ in 0..CAPACITY {
            let value = next_value;
            next_value += 1;
            assert_eq!(producer.try_push(value), Ok(()), "lap {lap}");
            shadow.push_back(value);
        }
        // Full: the offered value must come back untouched.
        let overflow = next_value;
        next_value += 1;
        assert_eq!(producer.try_push(overflow), Err(overflow), "lap {lap}");
        assert_eq!(shadow.len(), CAPACITY);

        // Free two slots, then refill across the wrapped tail index.
        for _ in 0..2 {
            pop_matches_shadow::<CAPACITY>(&mut consumer, &mut shadow, "lap refill");
        }
        for _ in 0..2 {
            let value = next_value;
            next_value += 1;
            assert_eq!(producer.try_push(value), Ok(()), "lap {lap}");
            shadow.push_back(value);
        }
        // Drain the lap completely.
        while pop_matches_shadow::<CAPACITY>(&mut consumer, &mut shadow, "lap drain") {}
    }

    assert!(shadow.is_empty());
}

#[test]
fn rejected_push_returns_bit_identical_value() {
    const PATTERNS: [u64; 5] = [
        0,
        u64::MAX,
        0xAAAA_AAAA_AAAA_AAAA,
        0x5555_5555_5555_5555,
        0x0000_0000_0000_00FF,
    ];

    for pattern in PATTERNS {
        let mut queue = SpscQueue::<u64, 2>::try_new().expect("valid capacity");
        let (mut producer, mut consumer) = queue.split();
        let mut shadow = VecDeque::from([0xFFFF_FFFF_FFFF_FF01, 0x0100_0000_0000_0000]);

        assert_eq!(producer.try_push(shadow[0]), Ok(()));
        assert_eq!(producer.try_push(shadow[1]), Ok(()));

        // Full: rejection must hand back the exact bits and leave state alone.
        assert_eq!(producer.try_push(pattern), Err(pattern));
        assert_eq!(shadow.len(), 2);

        // Make room, then the very same value must flow through losslessly.
        pop_matches_shadow::<2>(&mut consumer, &mut shadow, "make room");
        assert_eq!(producer.try_push(pattern), Ok(()));
        shadow.push_back(pattern);
        while pop_matches_shadow::<2>(&mut consumer, &mut shadow, "pattern flush") {}
        assert!(shadow.is_empty());
    }
}

#[test]
fn resplit_empty_queue_starts_at_published_positions() {
    let mut queue = SpscQueue::<u64, 2>::try_new().expect("valid capacity");
    {
        let (mut producer, mut consumer) = queue.split();
        assert_eq!(producer.try_push(11), Ok(()));
        assert_eq!(consumer.try_pop(), Some(11));
    }

    let (mut producer, mut consumer) = queue.split();
    assert_eq!(consumer.try_pop(), None);
    assert_eq!(producer.try_push(12), Ok(()));
    assert_eq!(consumer.try_pop(), Some(12));
}

#[test]
fn resplit_nonempty_queue_preserves_pending_values() {
    let mut queue = SpscQueue::<u64, 2>::try_new().expect("valid capacity");
    {
        let (mut producer, _consumer) = queue.split();
        assert_eq!(producer.try_push(21), Ok(()));
        assert_eq!(producer.try_push(22), Ok(()));
    }

    let (mut producer, mut consumer) = queue.split();
    assert_eq!(producer.try_push(23), Err(23));
    assert_eq!(consumer.try_pop(), Some(21));
    assert_eq!(producer.try_push(23), Ok(()));
    assert_eq!(consumer.try_pop(), Some(22));
    assert_eq!(consumer.try_pop(), Some(23));
    assert_eq!(consumer.try_pop(), None);
}

#[test]
fn into_inner_drains_only_pending_values() {
    let mut queue = SpscQueue::<u64, 4>::try_new().expect("valid capacity");
    {
        let (mut producer, mut consumer) = queue.split();
        assert_eq!(producer.try_push(24), Ok(()));
        assert_eq!(producer.try_push(25), Ok(()));
        assert_eq!(consumer.try_pop(), Some(24));
    }

    assert_eq!(queue.into_inner(), [25]);
}

#[test]
fn second_endpoint_epoch_moves_values_between_threads() {
    let mut queue = SpscQueue::<u64, 2>::try_new().expect("valid capacity");
    {
        let (mut producer, mut consumer) = queue.split();
        assert_eq!(producer.try_push(31), Ok(()));
        assert_eq!(consumer.try_pop(), Some(31));
    }

    let consumer_checked = AtomicBool::new(false);
    let producer_published = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let (mut producer, mut consumer) = queue.split();
        let producer_checked = &consumer_checked;
        let published_by_producer = &producer_published;
        let producer_thread = scope.spawn(move || {
            while !producer_checked.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
            let result = producer.try_push(32);
            published_by_producer.store(true, Ordering::Release);
            result
        });
        let checked_by_consumer = &consumer_checked;
        let consumer_published = &producer_published;
        let consumer_thread = scope.spawn(move || {
            let before_publication = consumer.try_pop();
            checked_by_consumer.store(true, Ordering::Release);
            while !consumer_published.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
            (before_publication, consumer.try_pop())
        });

        assert_eq!(producer_thread.join().expect("producer thread"), Ok(()));
        assert_eq!(
            consumer_thread.join().expect("consumer thread"),
            (None, Some(32))
        );
    });
}

/// Non-Copy probe: no Clone or Copy, so surviving a push/pop round trip
/// proves the queue moves values rather than duplicating or losing them.
struct Payload<'counter> {
    id: u64,
    drops: &'counter Cell<usize>,
}

impl Drop for Payload<'_> {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
    }
}

#[test]
fn values_drop_exactly_once_after_full_drain() {
    let drops = Cell::new(0_usize);
    let payload = |id: u64| Payload { id, drops: &drops };

    {
        let mut queue = SpscQueue::<Payload, 4>::try_new().expect("valid capacity");
        let (mut producer, mut consumer) = queue.split();

        for id in 0..4_u64 {
            assert!(matches!(producer.try_push(payload(id)), Ok(())));
        }
        let rejected = match producer.try_push(payload(4)) {
            Ok(()) => panic!("queue unexpectedly accepted beyond capacity"),
            Err(value) => value,
        };
        assert_eq!(rejected.id, 4);
        assert_eq!(drops.get(), 0, "rejected push must not drop the value");

        assert!(matches!(consumer.try_pop(), Some(Payload { id: 0, .. })));
        assert!(matches!(producer.try_push(rejected), Ok(())));

        for expected_id in 1..5_u64 {
            match consumer.try_pop() {
                Some(value) => assert_eq!(value.id, expected_id),
                None => panic!("queue lost payload {expected_id}"),
            }
        }
        assert!(consumer.try_pop().is_none());
        assert_eq!(drops.get(), 5, "each delivered payload drops exactly once");
    }

    {
        let mut queue = SpscQueue::<Payload, 2>::try_new().expect("valid capacity");
        let (mut producer, _consumer) = queue.split();

        for id in [10_u64, 11] {
            assert!(matches!(producer.try_push(payload(id)), Ok(())));
        }
        assert_eq!(drops.get(), 5, "queued payloads must stay alive");
    }
    // Residual payloads drop exactly once at queue teardown.
    assert_eq!(drops.get(), 7);
}

#[test]
fn values_drop_exactly_once_across_endpoint_epochs() {
    let drops = Cell::new(0_usize);
    let payload = |id: u64| Payload { id, drops: &drops };
    let mut queue = SpscQueue::<Payload, 2>::try_new().expect("valid capacity");

    {
        let (mut producer, mut consumer) = queue.split();
        assert!(matches!(producer.try_push(payload(40)), Ok(())));
        assert!(matches!(producer.try_push(payload(41)), Ok(())));
        assert!(matches!(consumer.try_pop(), Some(Payload { id: 40, .. })));
    }
    assert_eq!(drops.get(), 1);

    {
        let (mut producer, mut consumer) = queue.split();
        assert!(matches!(consumer.try_pop(), Some(Payload { id: 41, .. })));
        assert!(matches!(producer.try_push(payload(42)), Ok(())));
    }
    assert_eq!(drops.get(), 2);

    drop(queue);
    assert_eq!(drops.get(), 3);
}
