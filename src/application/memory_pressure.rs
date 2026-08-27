use crate::domain::{MemoryPressureEvaluation, MemoryPressurePolicy, MemoryPressureTracker};

use super::{MemoryPressureSource, PortError};

/// Application use case which evaluates lightweight system pressure samples.
pub struct MemoryPressureMonitor<S> {
    source: S,
    tracker: MemoryPressureTracker,
}

impl<S: MemoryPressureSource> MemoryPressureMonitor<S> {
    #[must_use]
    pub const fn new(source: S, policy: MemoryPressurePolicy) -> Self {
        Self {
            source,
            tracker: MemoryPressureTracker::new(policy),
        }
    }

    /// Samples and classifies current system-wide memory pressure.
    ///
    /// # Errors
    ///
    /// Returns the adapter error when memory or PSI data cannot be read.
    pub fn poll(&mut self) -> Result<MemoryPressureEvaluation, PortError> {
        self.source
            .sample()
            .map(|sample| self.tracker.evaluate(sample))
    }
}
