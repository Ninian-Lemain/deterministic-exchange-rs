#![deny(unsafe_op_in_unsafe_fn)]

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicUsize, Ordering};

#[repr(align(64))]
struct CacheLineAtomic(AtomicUsize);

const _: () = assert!(core::mem::align_of::<CacheLineAtomic>() >= 64);
const _: () = assert!(core::mem::size_of::<CacheLineAtomic>() >= 64);

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
    slots: [UnsafeCell<MaybeUninit<T>>; N],
}

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
            slots: core::array::from_fn(|_| UnsafeCell::new(MaybeUninit::uninit())),
        })
    }

    pub fn split(&mut self) -> (Producer<'_, T, N>, Consumer<'_, T, N>) {
        let queue: &SpscQueue<T, N> = self;
        (
            Producer {
                queue,
                tail: 0,
                cached_head: 0,
            },
            Consumer {
                queue,
                head: 0,
                cached_tail: 0,
            },
        )
    }

    #[must_use]
    pub const fn capacity(&self) -> usize {
        N
    }
}

// SAFETY: only the producer writes a slot before publishing it, and only the
// consumer reads/drops that slot after acquiring the publication. Split
// requires exclusive queue access and creates exactly one endpoint of each
// kind. T must be transferable between those threads.
unsafe impl<T: Send, const N: usize> Sync for SpscQueue<T, N> {}

impl<T, const N: usize> Drop for SpscQueue<T, N> {
    fn drop(&mut self) {
        let head = *self.head.0.get_mut();
        let tail = *self.tail.0.get_mut();
        let mut position = head;
        while position != tail {
            let index = position & (N - 1);
            // SAFETY: exclusive `&mut self` prevents endpoint access. Exactly
            // the published half-open range [head, tail) is initialized.
            unsafe { self.slots[index].get_mut().assume_init_drop() };
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
    /// # Errors
    ///
    /// Returns ownership of `value` when the bounded queue is full.
    pub fn try_push(&mut self, value: T) -> Result<(), T> {
        if self.tail.wrapping_sub(self.cached_head) == N {
            self.cached_head = self.queue.head.0.load(Ordering::Acquire);
            if self.tail.wrapping_sub(self.cached_head) == N {
                return Err(value);
            }
        }
        let index = self.tail & (N - 1);
        // SAFETY: the capacity check proves this slot was reclaimed. Only this
        // producer writes it, and it is not published until the Release store.
        unsafe { (*self.queue.slots[index].get()).write(value) };
        self.tail = self.tail.wrapping_add(1);
        self.queue.tail.0.store(self.tail, Ordering::Release);
        Ok(())
    }
}

pub struct Consumer<'queue, T, const N: usize> {
    queue: &'queue SpscQueue<T, N>,
    head: usize,
    cached_tail: usize,
}

impl<T, const N: usize> Consumer<'_, T, N> {
    pub fn try_pop(&mut self) -> Option<T> {
        if self.head == self.cached_tail {
            self.cached_tail = self.queue.tail.0.load(Ordering::Acquire);
            if self.head == self.cached_tail {
                return None;
            }
        }
        let index = self.head & (N - 1);
        // SAFETY: the Acquire load observed publication of this initialized
        // slot. Only this consumer reads it, exactly once, before reclamation.
        let value = unsafe { (*self.queue.slots[index].get()).assume_init_read() };
        self.head = self.head.wrapping_add(1);
        self.queue.head.0.store(self.head, Ordering::Release);
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_fifo_and_rejects_full() {
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

    #[test]
    fn moves_values_between_threads() {
        let mut queue = SpscQueue::<u64, 64>::try_new().expect("valid capacity");
        std::thread::scope(|scope| {
            let (mut producer, mut consumer) = queue.split();
            let sender = scope.spawn(move || {
                for value in 0..10_000 {
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
                for expected in 0..10_000 {
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
    fn loom_models_release_acquire_publication() {
        use loom::sync::Arc;
        use loom::sync::atomic::{AtomicUsize as LoomAtomicUsize, Ordering as LoomOrdering};
        use loom::thread;

        loom::model(|| {
            struct ModelQueue {
                head: LoomAtomicUsize,
                tail: LoomAtomicUsize,
                slot: LoomAtomicUsize,
            }

            let queue = Arc::new(ModelQueue {
                head: LoomAtomicUsize::new(0),
                tail: LoomAtomicUsize::new(0),
                slot: LoomAtomicUsize::new(0),
            });
            let producer_queue = Arc::clone(&queue);
            let producer = thread::spawn(move || {
                let head = producer_queue.head.load(LoomOrdering::Acquire);
                if producer_queue.tail.load(LoomOrdering::Relaxed) - head < 1 {
                    producer_queue.slot.store(7, LoomOrdering::Relaxed);
                    producer_queue.tail.store(1, LoomOrdering::Release);
                }
            });
            let consumer = thread::spawn(move || {
                let tail = queue.tail.load(LoomOrdering::Acquire);
                if queue.head.load(LoomOrdering::Relaxed) < tail {
                    assert_eq!(queue.slot.load(LoomOrdering::Relaxed), 7);
                    queue.head.store(1, LoomOrdering::Release);
                }
            });
            producer.join().expect("producer");
            consumer.join().expect("consumer");
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
