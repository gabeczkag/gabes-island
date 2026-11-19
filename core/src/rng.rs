//! Tiny deterministic xorshift64* RNG.
//!
//! The Linux app uses the same xorshift; the seed is derived from the clock so
//! runs differ, but tests can pin it.

#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    /// Seeds from the wall clock; still deterministic enough to be cheap.
    pub fn from_time() -> Rng {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x1234_5678);
        Rng::new(nanos ^ (std::process::id() as u64) << 32)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    /// Uniform in `[0, n)`; `0` when `n == 0`.
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next_u64() % n
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_for_a_seed() {
        let mut a = Rng::new(99);
        let mut b = Rng::new(99);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn zero_seed_is_not_stuck() {
        let mut r = Rng::new(0);
        let first = r.next_u64();
        assert_ne!(first, 0);
        assert_ne!(r.next_u64(), first);
    }

    #[test]
    fn below_is_bounded() {
        let mut r = Rng::new(5);
        for _ in 0..10_000 {
            assert!(r.below(7) < 7);
        }
        assert_eq!(r.below(0), 0);
        assert_eq!(r.below(1), 0);
    }

    #[test]
    fn spread_is_reasonable() {
        let mut r = Rng::new(1234);
        let mut buckets = [0u32; 10];
        for _ in 0..10_000 {
            buckets[r.below(10) as usize] += 1;
        }
        assert!(buckets.iter().all(|c| *c > 700 && *c < 1300), "{buckets:?}");
    }
}
