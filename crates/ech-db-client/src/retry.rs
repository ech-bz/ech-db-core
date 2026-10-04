use std::time::Duration;

use rand::Rng;

pub struct Backoff {
    attempt: u32,
}

impl Backoff {
    pub fn new() -> Self {
        Self { attempt: 0 }
    }

    pub fn delay(&mut self) -> Duration {
        let cap = 10u64
            .saturating_mul(2u64.checked_pow(self.attempt.min(63)).unwrap_or(u64::MAX))
            .min(1000);
        self.attempt = self.attempt.saturating_add(1);
        Duration::from_millis(rand::rng().random_range(0..=cap))
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}
