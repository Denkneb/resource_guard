use std::path::PathBuf;

/// Stable identity of a process for the duration of its lifetime.
///
/// A PID alone is not sufficient because Linux can reuse it after a process exits.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ProcessIdentity {
    pid: u32,
    uid: u32,
    started_at: u64,
}

impl ProcessIdentity {
    #[must_use]
    pub const fn new(pid: u32, uid: u32, started_at: u64) -> Self {
        Self {
            pid,
            uid,
            started_at,
        }
    }

    #[must_use]
    pub const fn pid(self) -> u32 {
        self.pid
    }

    #[must_use]
    pub const fn uid(self) -> u32 {
        self.uid
    }

    #[must_use]
    pub const fn started_at(self) -> u64 {
        self.started_at
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessDescriptor {
    identity: ProcessIdentity,
    name: String,
    executable: Option<PathBuf>,
}

impl ProcessDescriptor {
    #[must_use]
    pub fn new(
        identity: ProcessIdentity,
        name: impl Into<String>,
        executable: Option<PathBuf>,
    ) -> Self {
        Self {
            identity,
            name: name.into(),
            executable,
        }
    }

    #[must_use]
    pub const fn identity(&self) -> ProcessIdentity {
        self.identity
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn executable(&self) -> Option<&std::path::Path> {
        self.executable.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::ProcessIdentity;

    #[test]
    fn reused_pid_has_a_different_identity() {
        let original = ProcessIdentity::new(42, 1_000, 100);
        let reused = ProcessIdentity::new(42, 1_000, 101);

        assert_ne!(original, reused);
    }
}
