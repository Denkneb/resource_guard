use std::time::{Duration, Instant};

use crate::application::MonotonicClock;

#[derive(Clone, Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl MonotonicClock for SystemClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::application::MonotonicClock;

    use super::SystemClock;

    #[test]
    fn starts_from_a_local_monotonic_origin() {
        let clock = SystemClock::new();

        assert!(clock.now() < Duration::from_secs(1));
    }
}
