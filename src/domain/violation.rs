use std::{collections::HashMap, time::Duration};

use super::{ProcessIdentity, ResourceBreach};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ViolationPolicy {
    pub consecutive_samples: u32,
    pub minimum_duration: Duration,
    pub cooldown: Duration,
}

impl ViolationPolicy {
    #[must_use]
    pub const fn new(
        consecutive_samples: u32,
        minimum_duration: Duration,
        cooldown: Duration,
    ) -> Self {
        Self {
            consecutive_samples,
            minimum_duration,
            cooldown,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Evaluation {
    Normal,
    Pending {
        consecutive_samples: u32,
        elapsed: Duration,
    },
    Notify {
        breach: ResourceBreach,
        consecutive_samples: u32,
        elapsed: Duration,
    },
    Cooldown {
        remaining: Duration,
    },
}

#[derive(Clone, Copy, Debug)]
struct ViolationState {
    first_seen_at: Duration,
    consecutive_samples: u32,
    last_notified_at: Option<Duration>,
}

#[derive(Debug)]
pub struct ViolationTracker {
    policy: ViolationPolicy,
    states: HashMap<ProcessIdentity, ViolationState>,
}

impl ViolationTracker {
    #[must_use]
    pub fn new(policy: ViolationPolicy) -> Self {
        Self {
            policy,
            states: HashMap::new(),
        }
    }

    pub fn evaluate(
        &mut self,
        identity: ProcessIdentity,
        breach: ResourceBreach,
        now: Duration,
    ) -> Evaluation {
        if !breach.any() {
            self.states.remove(&identity);
            return Evaluation::Normal;
        }

        let state = self.states.entry(identity).or_insert(ViolationState {
            first_seen_at: now,
            consecutive_samples: 0,
            last_notified_at: None,
        });
        state.consecutive_samples = state.consecutive_samples.saturating_add(1);

        let elapsed = now.saturating_sub(state.first_seen_at);
        if state.consecutive_samples < self.policy.consecutive_samples
            || elapsed < self.policy.minimum_duration
        {
            return Evaluation::Pending {
                consecutive_samples: state.consecutive_samples,
                elapsed,
            };
        }

        if let Some(last_notified_at) = state.last_notified_at {
            let since_notification = now.saturating_sub(last_notified_at);
            if since_notification < self.policy.cooldown {
                return Evaluation::Cooldown {
                    remaining: self.policy.cooldown.saturating_sub(since_notification),
                };
            }
        }

        state.last_notified_at = Some(now);
        Evaluation::Notify {
            breach,
            consecutive_samples: state.consecutive_samples,
            elapsed,
        }
    }

    pub fn forget(&mut self, identity: ProcessIdentity) {
        self.states.remove(&identity);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Evaluation, ViolationPolicy, ViolationTracker};
    use crate::domain::{ProcessIdentity, ResourceBreach};

    const IDENTITY: ProcessIdentity = ProcessIdentity::new(42, 1_000, 100);
    const BREACH: ResourceBreach = ResourceBreach {
        cpu: true,
        memory: false,
    };

    fn tracker() -> ViolationTracker {
        ViolationTracker::new(ViolationPolicy::new(
            3,
            Duration::from_secs(10),
            Duration::from_secs(60),
        ))
    }

    #[test]
    fn requires_consecutive_samples_and_minimum_duration() {
        let mut tracker = tracker();

        assert!(matches!(
            tracker.evaluate(IDENTITY, BREACH, Duration::ZERO),
            Evaluation::Pending {
                consecutive_samples: 1,
                ..
            }
        ));
        assert!(matches!(
            tracker.evaluate(IDENTITY, BREACH, Duration::from_secs(5)),
            Evaluation::Pending {
                consecutive_samples: 2,
                ..
            }
        ));
        assert!(matches!(
            tracker.evaluate(IDENTITY, BREACH, Duration::from_secs(10)),
            Evaluation::Notify {
                consecutive_samples: 3,
                ..
            }
        ));
    }

    #[test]
    fn normal_sample_resets_a_short_spike() {
        let mut tracker = tracker();

        tracker.evaluate(IDENTITY, BREACH, Duration::ZERO);
        assert_eq!(
            tracker.evaluate(IDENTITY, ResourceBreach::default(), Duration::from_secs(5)),
            Evaluation::Normal
        );
        assert!(matches!(
            tracker.evaluate(IDENTITY, BREACH, Duration::from_secs(10)),
            Evaluation::Pending {
                consecutive_samples: 1,
                elapsed: Duration::ZERO,
            }
        ));
    }

    #[test]
    fn suppresses_repeated_notifications_during_cooldown() {
        let mut tracker = tracker();
        tracker.evaluate(IDENTITY, BREACH, Duration::ZERO);
        tracker.evaluate(IDENTITY, BREACH, Duration::from_secs(5));
        tracker.evaluate(IDENTITY, BREACH, Duration::from_secs(10));

        assert_eq!(
            tracker.evaluate(IDENTITY, BREACH, Duration::from_secs(20)),
            Evaluation::Cooldown {
                remaining: Duration::from_secs(50),
            }
        );
        assert!(matches!(
            tracker.evaluate(IDENTITY, BREACH, Duration::from_secs(70)),
            Evaluation::Notify { .. }
        ));
    }

    #[test]
    fn reused_pid_starts_an_independent_violation_series() {
        let reused = ProcessIdentity::new(IDENTITY.pid(), IDENTITY.uid(), 101);
        let mut tracker = tracker();
        tracker.evaluate(IDENTITY, BREACH, Duration::ZERO);
        tracker.evaluate(IDENTITY, BREACH, Duration::from_secs(5));

        assert!(matches!(
            tracker.evaluate(reused, BREACH, Duration::from_secs(10)),
            Evaluation::Pending {
                consecutive_samples: 1,
                elapsed: Duration::ZERO,
            }
        ));
    }
}
