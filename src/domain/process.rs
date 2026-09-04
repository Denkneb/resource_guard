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
    parent_pid: Option<u32>,
    state: ProcessState,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProcessState {
    Running,
    Sleeping,
    Uninterruptible,
    Zombie,
    #[default]
    Other,
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
            parent_pid: None,
            state: ProcessState::Other,
        }
    }

    #[must_use]
    pub const fn with_runtime(mut self, parent_pid: Option<u32>, state: ProcessState) -> Self {
        self.parent_pid = parent_pid;
        self.state = state;
        self
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

    #[must_use]
    pub const fn parent_pid(&self) -> Option<u32> {
        self.parent_pid
    }

    #[must_use]
    pub const fn state(&self) -> ProcessState {
        self.state
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
