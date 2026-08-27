use std::{fs, path::PathBuf};

use sysinfo::System;

use crate::{
    application::{MemoryPressureSource, PortError},
    domain::{MemoryPressureSample, MemoryPsi, SystemResources},
};

const PSI_MEMORY_PATH: &str = "/proc/pressure/memory";

/// Lightweight Linux memory-pressure adapter backed by sysinfo and PSI.
#[derive(Debug)]
pub struct ProcMemoryPressureSource {
    system: System,
    psi_path: PathBuf,
}

impl ProcMemoryPressureSource {
    #[must_use]
    pub fn new() -> Self {
        Self {
            system: System::new(),
            psi_path: PathBuf::from(PSI_MEMORY_PATH),
        }
    }

    #[cfg(test)]
    fn with_psi_path(path: impl Into<PathBuf>) -> Self {
        Self {
            system: System::new(),
            psi_path: path.into(),
        }
    }
}

impl Default for ProcMemoryPressureSource {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryPressureSource for ProcMemoryPressureSource {
    fn sample(&mut self) -> Result<MemoryPressureSample, PortError> {
        self.system.refresh_memory();
        let contents = fs::read_to_string(&self.psi_path)
            .map_err(|error| PortError::new("read memory PSI", error.to_string()))?;
        let psi = parse_memory_psi(&contents)?;

        Ok(MemoryPressureSample {
            system: SystemResources {
                total_memory_bytes: self.system.total_memory(),
                available_memory_bytes: self.system.available_memory(),
                total_swap_bytes: self.system.total_swap(),
                used_swap_bytes: self.system.used_swap(),
            },
            psi,
        })
    }
}

fn parse_memory_psi(contents: &str) -> Result<MemoryPsi, PortError> {
    let some_avg10 = parse_avg10(contents, "some")?;
    let full_avg10 = parse_avg10(contents, "full")?;
    Ok(MemoryPsi {
        some_avg10,
        full_avg10,
    })
}

fn parse_avg10(contents: &str, category: &'static str) -> Result<f32, PortError> {
    let line = contents
        .lines()
        .find(|line| line.split_whitespace().next() == Some(category))
        .ok_or_else(|| PortError::new("parse memory PSI", format!("missing {category} line")))?;
    let value = line
        .split_whitespace()
        .find_map(|field| field.strip_prefix("avg10="))
        .ok_or_else(|| PortError::new("parse memory PSI", format!("missing {category} avg10")))?;
    let parsed = value
        .parse::<f32>()
        .map_err(|error| PortError::new("parse memory PSI", error.to_string()))?;
    if !parsed.is_finite() || !(0.0..=100.0).contains(&parsed) {
        return Err(PortError::new(
            "parse memory PSI",
            format!("invalid {category} avg10"),
        ));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{ProcMemoryPressureSource, parse_memory_psi};
    use crate::application::MemoryPressureSource;

    #[test]
    fn parses_some_and_full_avg10() {
        let psi = parse_memory_psi(
            "some avg10=12.34 avg60=2.00 avg300=1.00 total=10\n\
             full avg10=5.67 avg60=1.00 avg300=0.50 total=5\n",
        )
        .unwrap();

        assert!((psi.some_avg10 - 12.34).abs() < f32::EPSILON);
        assert!((psi.full_avg10 - 5.67).abs() < f32::EPSILON);
    }

    #[test]
    fn rejects_incomplete_psi_data() {
        assert!(parse_memory_psi("some avg10=1.0 total=1\n").is_err());
    }

    #[test]
    fn samples_memory_with_a_controlled_psi_file() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("memory.pressure");
        fs::write(
            &path,
            "some avg10=1.00 avg60=0.00 avg300=0.00 total=1\n\
             full avg10=0.25 avg60=0.00 avg300=0.00 total=1\n",
        )
        .unwrap();
        let mut source = ProcMemoryPressureSource::with_psi_path(path);

        let sample = source.sample().unwrap();

        assert!(sample.system.total_memory_bytes > 0);
        assert!((sample.psi.full_avg10 - 0.25).abs() < f32::EPSILON);
    }
}
