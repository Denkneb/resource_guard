use std::path::{Path, PathBuf};

/// Neutral description of where a process came from.
///
/// The Linux adapter derives this from the cgroup path so domain and
/// application layers never see raw systemd syntax.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProcessOrigin {
    UserApplication,
    UserService,
    #[default]
    Unknown,
}

/// Execution context of a process independent of any Linux cgroup format.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProcessExecutionContext {
    group_id: Option<String>,
    unit_name: Option<String>,
    origin: ProcessOrigin,
    has_controlling_terminal: bool,
}

impl ProcessExecutionContext {
    #[must_use]
    pub fn new(
        group_id: Option<String>,
        unit_name: Option<String>,
        origin: ProcessOrigin,
        has_controlling_terminal: bool,
    ) -> Self {
        Self {
            group_id,
            unit_name,
            origin,
            has_controlling_terminal,
        }
    }

    #[must_use]
    pub fn group_id(&self) -> Option<&str> {
        self.group_id.as_deref()
    }

    #[must_use]
    pub fn unit_name(&self) -> Option<&str> {
        self.unit_name.as_deref()
    }

    #[must_use]
    pub const fn origin(&self) -> ProcessOrigin {
        self.origin
    }

    #[must_use]
    pub const fn has_controlling_terminal(&self) -> bool {
        self.has_controlling_terminal
    }
}

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
    execution_context: ProcessExecutionContext,
    working_directory: Option<PathBuf>,
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
            execution_context: ProcessExecutionContext::default(),
            working_directory: None,
        }
    }

    #[must_use]
    pub const fn with_runtime(mut self, parent_pid: Option<u32>, state: ProcessState) -> Self {
        self.parent_pid = parent_pid;
        self.state = state;
        self
    }

    #[must_use]
    pub fn with_execution_context(mut self, context: ProcessExecutionContext) -> Self {
        self.execution_context = context;
        self
    }

    /// Sets an already validated absolute working directory.
    ///
    /// The Linux adapter reads `/proc/<pid>/cwd` and never exposes the raw
    /// system call to domain or application code.
    #[must_use]
    pub fn with_working_directory(mut self, working_directory: Option<PathBuf>) -> Self {
        self.working_directory = working_directory;
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

    #[must_use]
    pub const fn execution_context(&self) -> &ProcessExecutionContext {
        &self.execution_context
    }

    #[must_use]
    pub fn working_directory(&self) -> Option<&Path> {
        self.working_directory.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{ProcessDescriptor, ProcessIdentity};

    #[test]
    fn reused_pid_has_a_different_identity() {
        let original = ProcessIdentity::new(42, 1_000, 100);
        let reused = ProcessIdentity::new(42, 1_000, 101);

        assert_ne!(original, reused);
    }

    #[test]
    fn working_directory_defaults_to_none_and_is_settable() {
        let identity = ProcessIdentity::new(42, 1_000, 100);
        let without = ProcessDescriptor::new(identity, "worker", None);
        assert_eq!(without.working_directory(), None);

        let with = without.with_working_directory(Some(PathBuf::from("/work/project")));
        assert_eq!(
            with.working_directory(),
            Some(std::path::Path::new("/work/project"))
        );
    }
}
