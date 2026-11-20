use core::sync::atomic::{AtomicU64, Ordering, fence};

const WRITER_BIT: u64 = 1;
const STEP: u64 = 2;

/// Sequence lock: locking for very read heavy workloads
/// - unbounded parallel reads
/// - writes can be requested in parallel but are serialized by spinlocks
/// - will cause reads to retry
#[repr(transparent)]
pub struct SeqLock {
    version: AtomicU64,
}

impl Default for SeqLock {
    fn default() -> Self {
        Self::new()
    }
}

impl SeqLock {
    pub const fn new() -> Self {
        Self {
            version: AtomicU64::new(0),
        }
    }

    /// Initializes the lock before first use.
    pub fn reset(&self) {
        self.version.store(0, Ordering::Relaxed);
    }

    /// Calls `f` until no write overlaps.
    #[inline(always)]
    pub fn read<T>(&self, f: impl Fn() -> T) -> T {
        loop {
            let start = self.version.load(Ordering::Acquire);
            if start & WRITER_BIT != 0 {
                core::hint::spin_loop();
                continue;
            }

            let value = f();

            // Order the protected loads before the second version read.
            fence(Ordering::Acquire);

            if self.version.load(Ordering::Relaxed) == start {
                return value;
            }
        }
    }

    /// Runs `f` with the writer lock held and publishes its result.
    #[inline(always)]
    pub fn write<T>(&self, f: impl FnOnce() -> T) -> T {
        let version = self.acquire();
        let value = f();
        self.version
            .store(version.wrapping_add(STEP), Ordering::Release);
        value
    }

    /// Takes the writer bit and returns the previous even version.
    #[inline(always)]
    fn acquire(&self) -> u64 {
        let mut version = self.version.load(Ordering::Relaxed);
        loop {
            if version & WRITER_BIT == 0 {
                match self.version.compare_exchange_weak(
                    version,
                    version | WRITER_BIT,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return version,
                    Err(observed) => version = observed,
                }
            } else {
                core::hint::spin_loop();
                version = self.version.load(Ordering::Relaxed);
            }
        }
    }
}
