#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicUsize, Ordering};
#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicUsize, Ordering};

#[cfg(not(feature = "loom"))]
use core::cell::UnsafeCell as SlotUnsafeCell;
use core::mem::MaybeUninit;
#[cfg(feature = "loom")]
use loom::cell::UnsafeCell as SlotUnsafeCell;

struct Slot<T>(SlotUnsafeCell<MaybeUninit<T>>);

impl<T> Slot<T> {
    fn uninit() -> Self {
        Self(SlotUnsafeCell::new(MaybeUninit::uninit()))
    }

    /// Writes a value into a reclaimed slot.
    ///
    /// # Safety
    ///
    /// The slot must be uninitialized and inaccessible to the consumer.
    unsafe fn write(&self, value: T) {
        #[cfg(not(feature = "loom"))]
        unsafe {
            (*self.0.get()).write(value);
        }
        #[cfg(feature = "loom")]
        self.0.with_mut(|slot| unsafe {
            (*slot).write(value);
        });
    }

    /// Moves a published value out of a slot.
    ///
    /// # Safety
    ///
    /// The slot must contain an initialized value and no writer may access it.
    unsafe fn read(&self) -> T {
        #[cfg(not(feature = "loom"))]
        unsafe {
            (*self.0.get()).assume_init_read()
        }
        #[cfg(feature = "loom")]
        {
            self.0.with(|slot| unsafe { (*slot).assume_init_read() })
        }
    }

    /// Drops the initialized value in an exclusively owned slot.
    ///
    /// # Safety
    ///
    /// The slot must contain an initialized value.
    unsafe fn drop_value(&mut self) {
        #[cfg(not(feature = "loom"))]
        unsafe {
            self.0.get_mut().assume_init_drop();
        }
        #[cfg(feature = "loom")]
        self.0.with_mut(|slot| unsafe {
            (*slot).assume_init_drop();
        });
    }
}

#[repr(align(64))]
struct CacheLineAtomic(AtomicUsize);

const _: () = assert!(core::mem::align_of::<CacheLineAtomic>() >= 64);
const _: () = assert!(core::mem::size_of::<CacheLineAtomic>() >= 64);

impl CacheLineAtomic {
    /// Exclusive read for owner-only paths (`split`, `Drop`, `into_inner`).
    fn exclusive_load(&mut self) -> usize {
        self.0.load(Ordering::Relaxed)
    }

    /// Exclusive write for owner-only paths.
    fn exclusive_store(&mut self, value: usize) {
        #[cfg(feature = "loom")]
        {
            self.0.store(value, Ordering::Relaxed);
        }
        #[cfg(not(feature = "loom"))]
        {
            *self.0.get_mut() = value;
        }
    }
}

/// A fixed-capacity, single-producer/single-consumer ring.
///
/// `N` must be a non-zero power of two. The producer exclusively writes
/// `tail`; the consumer exclusively writes `head`. A producer publishes a
/// fully initialized slot with a Release store to `tail`, paired with the
/// consumer's Acquire load. The consumer publishes slot reclamation with a
/// Release store to `head`, paired with the producer's Acquire load. Cached
/// peer positions are thread-private and need no atomics.
pub struct SpscQueue<T, const N: usize> {
    head: CacheLineAtomic,
    tail: CacheLineAtomic,
    slots: [Slot<T>; N],
}

/// True when this build swapped in Loom primitives (`--features loom`).
/// Allocation-gated suites must skip themselves on such builds.
pub const IS_LOOM_BUILD: bool = cfg!(feature = "loom");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueConfigError {
    CapacityMustBePowerOfTwo,
}

impl<T, const N: usize> SpscQueue<T, N> {
    /// # Errors
    ///
    /// Returns [`QueueConfigError::CapacityMustBePowerOfTwo`] unless `N` is a
    /// non-zero power of two.
    pub fn try_new() -> Result<Self, QueueConfigError> {
        if !N.is_power_of_two() {
            return Err(QueueConfigError::CapacityMustBePowerOfTwo);
        }
        Ok(Self {
            head: CacheLineAtomic(AtomicUsize::new(0)),
            tail: CacheLineAtomic(AtomicUsize::new(0)),
            slots: core::array::from_fn(|_| Slot::uninit()),
        })
    }

    pub fn split(&mut self) -> (Producer<'_, T, N>, Consumer<'_, T, N>) {
        let head = self.head.exclusive_load();
        let tail = self.tail.exclusive_load();
        let queue: &SpscQueue<T, N> = self;
        (
            Producer {
                queue,
                tail,
                cached_head: head,
            },
            Consumer {
                queue,
                head,
                cached_tail: tail,
            },
        )
    }

    #[must_use]
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Drains every published-but-unconsumed value, leaving the queue empty.
    #[must_use]
    pub fn into_inner(mut self) -> std::vec::Vec<T> {
        let mut position = self.head.exclusive_load();
        let tail = self.tail.exclusive_load();
        let mut drained = std::vec::Vec::with_capacity(tail.wrapping_sub(position));
        while position != tail {
            let index = position & (N - 1);
            position = position.wrapping_add(1);
            // Remove the slot from queue state before moving its value. Drop
            // must not visit the same slot if draining unwinds.
            self.head.exclusive_store(position);
            // SAFETY: exclusive `&mut self`; exactly [head, tail) is live.
            let value = unsafe { self.slots[index].read() };
            drained.push(value);
        }
        drained
    }

    /// Core producer step on shared references. `tail` and `cached_head` are
    /// the calling endpoint's private cursor and peer cache. The public
    /// [`Producer::try_push`] delegates with its own state; Loom tests call
    /// this directly from two modeled threads sharing one queue.
    fn push_impl(&self, tail: &mut usize, cached_head: &mut usize, value: T) -> Result<(), T> {
        if tail.wrapping_sub(*cached_head) == N {
            *cached_head = self.head.0.load(Ordering::Acquire);
            if tail.wrapping_sub(*cached_head) == N {
                return Err(value);
            }
        }
        let index = *tail & (N - 1);
        // SAFETY: the capacity check proves this slot was reclaimed. Only this
        // producer writes it, and it is not published until the Release store.
        unsafe { self.slots[index].write(value) };
        *tail = tail.wrapping_add(1);
        self.tail.0.store(*tail, Ordering::Release);
        Ok(())
    }

    /// Core consumer step on shared references; see [`Self::push_impl`].
    fn pop_impl(&self, head: &mut usize, cached_tail: &mut usize) -> Option<T> {
        if *head == *cached_tail {
            *cached_tail = self.tail.0.load(Ordering::Acquire);
            if *head == *cached_tail {
                return None;
            }
        }
        let index = *head & (N - 1);
        // SAFETY: the Acquire load observed publication of this initialized
        // slot. Only this consumer reads it, exactly once, before reclamation.
        let value = unsafe { self.slots[index].read() };
        *head = head.wrapping_add(1);
        self.head.0.store(*head, Ordering::Release);
        Some(value)
    }
}

// SAFETY: only the producer writes a slot before publishing it, and only the
// consumer reads/drops that slot after acquiring the publication. Split
// requires exclusive queue access and creates exactly one endpoint of each
// kind. T must be transferable between those threads.
unsafe impl<T: Send, const N: usize> Sync for SpscQueue<T, N> {}

impl<T, const N: usize> Drop for SpscQueue<T, N> {
    fn drop(&mut self) {
        let head = self.head.exclusive_load();
        let tail = self.tail.exclusive_load();
        let mut position = head;
        while position != tail {
            let index = position & (N - 1);
            // SAFETY: exclusive `&mut self` prevents endpoint access. Exactly
            // the published half-open range [head, tail) is initialized.
            unsafe { self.slots[index].drop_value() };
            position = position.wrapping_add(1);
        }
    }
}

pub struct Producer<'queue, T, const N: usize> {
    queue: &'queue SpscQueue<T, N>,
    tail: usize,
    cached_head: usize,
}

impl<T, const N: usize> Producer<'_, T, N> {
    /// Returns whether one value can be published without backpressure.
    ///
    /// A `true` result remains valid until this producer publishes a value.
    /// The consumer can only reclaim more capacity.
    pub fn has_capacity(&mut self) -> bool {
        if self.tail.wrapping_sub(self.cached_head) == N {
            self.cached_head = self.queue.head.0.load(Ordering::Acquire);
        }
        self.tail.wrapping_sub(self.cached_head) != N
    }

    /// # Errors
    ///
    /// Returns ownership of `value` when the bounded queue is full.
    pub fn try_push(&mut self, value: T) -> Result<(), T> {
        let Self {
            queue,
            tail,
            cached_head,
        } = self;
        queue.push_impl(tail, cached_head, value)
    }
}

pub struct Consumer<'queue, T, const N: usize> {
    queue: &'queue SpscQueue<T, N>,
    head: usize,
    cached_tail: usize,
}

impl<T, const N: usize> Consumer<'_, T, N> {
    pub fn try_pop(&mut self) -> Option<T> {
        let Self {
            queue,
            head,
            cached_tail,
        } = self;
        queue.pop_impl(head, cached_tail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_fifo_and_rejects_full() {
        // Under a Loom build the endpoints use Loom primitives, which are
        // only legal inside `loom::model`; run there too so the same
        // assertions cover the swapped build.
        #[cfg(feature = "loom")]
        loom::model(run_preserves_fifo);
        #[cfg(not(feature = "loom"))]
        run_preserves_fifo();
    }

    #[test]
    fn capacity_check_tracks_full_reclaim_and_wrap() {
        #[cfg(feature = "loom")]
        loom::model(run_capacity_check_tracks_full_reclaim_and_wrap);
        #[cfg(not(feature = "loom"))]
        run_capacity_check_tracks_full_reclaim_and_wrap();
    }

    fn run_capacity_check_tracks_full_reclaim_and_wrap() {
        let mut queue = SpscQueue::<u64, 1>::try_new().expect("valid capacity");
        let (mut producer, mut consumer) = queue.split();

        for value in 0..4 {
            assert!(producer.has_capacity());
            assert_eq!(producer.try_push(value), Ok(()));
            assert!(!producer.has_capacity());
            assert_eq!(consumer.try_pop(), Some(value));
            assert!(producer.has_capacity());
        }
        assert_eq!(consumer.try_pop(), None);
    }

    fn run_preserves_fifo() {
        let mut queue = SpscQueue::<u32, 2>::try_new().expect("valid capacity");
        let (mut producer, mut consumer) = queue.split();
        assert_eq!(producer.try_push(1), Ok(()));
        assert_eq!(producer.try_push(2), Ok(()));
        assert_eq!(producer.try_push(3), Err(3));
        assert_eq!(consumer.try_pop(), Some(1));
        assert_eq!(producer.try_push(3), Ok(()));
        assert_eq!(consumer.try_pop(), Some(2));
        assert_eq!(consumer.try_pop(), Some(3));
        assert_eq!(consumer.try_pop(), None);
    }

    #[cfg(not(feature = "loom"))]
    #[test]
    fn moves_values_between_threads() {
        let iterations = if cfg!(miri) { 100 } else { 10_000 };
        let mut queue = SpscQueue::<u64, 64>::try_new().expect("valid capacity");
        std::thread::scope(|scope| {
            let (mut producer, mut consumer) = queue.split();
            let sender = scope.spawn(move || {
                for value in 0..iterations {
                    let mut pending = value;
                    loop {
                        match producer.try_push(pending) {
                            Ok(()) => break,
                            Err(value) => {
                                pending = value;
                                core::hint::spin_loop();
                            }
                        }
                    }
                }
            });
            let receiver = scope.spawn(move || {
                for expected in 0..iterations {
                    loop {
                        if let Some(actual) = consumer.try_pop() {
                            assert_eq!(actual, expected);
                            break;
                        }
                        core::hint::spin_loop();
                    }
                }
            });
            sender.join().expect("sender thread");
            receiver.join().expect("receiver thread");
        });
    }

    #[cfg(feature = "loom")]
    #[test]
    fn loom_actual_queue_delivers_fifo_under_every_interleaving() {
        use loom::sync::Arc;
        use loom::thread;

        const VALUES: [u32; 3] = [1, 2, 3];
        loom::model(|| {
            let queue = Arc::new(SpscQueue::<u32, 2>::try_new().expect("capacity"));
            let producer_queue = Arc::clone(&queue);
            let consumer_queue = Arc::clone(&queue);

            let producer = thread::spawn(move || {
                let mut tail = 0_usize;
                let mut cached_head = 0_usize;
                for value in VALUES {
                    loop {
                        if producer_queue
                            .push_impl(&mut tail, &mut cached_head, value)
                            .is_ok()
                        {
                            break;
                        }
                        thread::yield_now();
                    }
                }
            });

            let consumer = thread::spawn(move || {
                let mut head = 0_usize;
                let mut cached_tail = 0_usize;
                let mut received = Vec::new();
                while received.len() < VALUES.len() {
                    if let Some(value) = consumer_queue.pop_impl(&mut head, &mut cached_tail) {
                        received.push(value);
                    } else {
                        thread::yield_now();
                    }
                }
                received
            });

            assert_eq!(
                consumer.join().expect("consumer").as_slice(),
                VALUES,
                "accepted values appear exactly once, in FIFO order"
            );
            producer.join().expect("producer");
            // Every pushed value was consumed: the ring ends empty.
            let Ok(owned) = Arc::try_unwrap(queue) else {
                panic!("queue handles released");
            };
            assert_eq!(owned.into_inner(), []);
        });
    }

    #[cfg(feature = "loom")]
    #[test]
    fn loom_actual_queue_capacity_one_backpressure_and_wrap() {
        use loom::sync::Arc;
        use loom::thread;

        // Phase A: a concurrent handoff of one value across capacity one
        // explores every publication/consumption interleaving.
        loom::model(|| {
            let queue = Arc::new(SpscQueue::<u64, 1>::try_new().expect("capacity"));
            let producer_queue = Arc::clone(&queue);
            let consumer_queue = Arc::clone(&queue);

            let producer = thread::spawn(move || {
                let mut tail = 0_usize;
                let mut cached_head = 0_usize;
                loop {
                    if producer_queue
                        .push_impl(&mut tail, &mut cached_head, u64::MAX)
                        .is_ok()
                    {
                        break;
                    }
                    thread::yield_now();
                }
            });
            let consumer = thread::spawn(move || {
                let mut head = 0_usize;
                let mut cached_tail = 0_usize;
                loop {
                    if consumer_queue.pop_impl(&mut head, &mut cached_tail) == Some(u64::MAX) {
                        break;
                    }
                    thread::yield_now();
                }
            });
            producer.join().expect("producer");
            consumer.join().expect("consumer");
            let Ok(owned) = Arc::try_unwrap(queue) else {
                panic!("queue handles released");
            };
            assert_eq!(owned.into_inner(), []);
        });

        // Phase B: deterministic full/backpressure/wrap without contention.
        // The rejected payload comes back bit-intact, the ring wraps its
        // indices through capacity one, and the final drain sees every value
        // exactly once.
        loom::model(|| {
            let mut queue = SpscQueue::<u64, 1>::try_new().expect("capacity");
            let (mut producer, mut consumer) = queue.split();
            assert_eq!(producer.try_push(u64::MAX), Ok(()));
            assert_eq!(producer.try_push(0xAAAA), Err(0xAAAA));
            assert_eq!(consumer.try_pop(), Some(u64::MAX));
            assert_eq!(producer.try_push(0xAAAA), Ok(()));
            assert_eq!(consumer.try_pop(), Some(0xAAAA));
            assert_eq!(consumer.try_pop(), None);
        });
    }

    #[test]
    fn invalid_capacity_is_explicit() {
        assert!(matches!(
            SpscQueue::<u8, 3>::try_new(),
            Err(QueueConfigError::CapacityMustBePowerOfTwo)
        ));
    }
}
