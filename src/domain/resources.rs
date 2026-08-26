use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProcessResources {
    pub cpu_percent: f32,
    pub resident_memory_bytes: u64,
    pub virtual_memory_bytes: u64,
    pub running_for: Duration,
    pub observed_at: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SystemResources {
    pub total_memory_bytes: u64,
    pub available_memory_bytes: u64,
    pub total_swap_bytes: u64,
    pub used_swap_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thresholds {
    pub max_cpu_percent: Option<f32>,
    pub max_resident_memory_bytes: Option<u64>,
}

impl Thresholds {
    #[must_use]
    pub fn evaluate(self, resources: ProcessResources) -> ResourceBreach {
        ResourceBreach {
            cpu: self
                .max_cpu_percent
                .is_some_and(|maximum| resources.cpu_percent > maximum),
            memory: self
                .max_resident_memory_bytes
                .is_some_and(|maximum| resources.resident_memory_bytes > maximum),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResourceBreach {
    pub cpu: bool,
    pub memory: bool,
}

impl ResourceBreach {
    #[must_use]
    pub const fn any(self) -> bool {
        self.cpu || self.memory
    }
}

#[cfg(test)]
mod tests {
    use super::{ProcessResources, ResourceBreach, Thresholds};
    use std::time::Duration;

    fn resources(cpu_percent: f32, resident_memory_bytes: u64) -> ProcessResources {
        ProcessResources {
            cpu_percent,
            resident_memory_bytes,
            virtual_memory_bytes: resident_memory_bytes * 2,
            running_for: Duration::from_secs(60),
            observed_at: Duration::ZERO,
        }
    }

    #[test]
    fn reports_each_exceeded_resource() {
        let thresholds = Thresholds {
            max_cpu_percent: Some(80.0),
            max_resident_memory_bytes: Some(1_024),
        };

        assert_eq!(
            thresholds.evaluate(resources(90.0, 2_048)),
            ResourceBreach {
                cpu: true,
                memory: true,
            }
        );
    }

    #[test]
    fn equality_with_a_limit_is_not_a_breach() {
        let thresholds = Thresholds {
            max_cpu_percent: Some(80.0),
            max_resident_memory_bytes: Some(1_024),
        };

        assert!(!thresholds.evaluate(resources(80.0, 1_024)).any());
    }
}
