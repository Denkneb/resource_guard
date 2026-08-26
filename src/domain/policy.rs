use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};

use super::{ProcessDescriptor, ProcessIdentity};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessDisposition {
    Monitor,
    Ignore,
    Protect,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IgnoreRule {
    Name(String),
    Executable(PathBuf),
}

impl IgnoreRule {
    #[must_use]
    pub fn for_process(process: &ProcessDescriptor) -> Self {
        process.executable().map_or_else(
            || Self::Name(process.name().to_owned()),
            |path| Self::Executable(path.to_path_buf()),
        )
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProtectionPolicy {
    protected_names: HashSet<String>,
    protected_executables: HashSet<PathBuf>,
    ignored_names: HashSet<String>,
    ignored_executables: HashSet<PathBuf>,
}

impl ProtectionPolicy {
    #[must_use]
    pub fn new(
        protected_names: impl IntoIterator<Item = String>,
        protected_executables: impl IntoIterator<Item = PathBuf>,
        ignored_names: impl IntoIterator<Item = String>,
        ignored_executables: impl IntoIterator<Item = PathBuf>,
    ) -> Self {
        Self {
            protected_names: protected_names.into_iter().collect(),
            protected_executables: protected_executables.into_iter().collect(),
            ignored_names: ignored_names.into_iter().collect(),
            ignored_executables: ignored_executables.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn disposition(&self, process: &ProcessDescriptor) -> ProcessDisposition {
        if self.protected_names.contains(process.name())
            || process
                .executable()
                .is_some_and(|path| self.protected_executables.contains(path))
        {
            ProcessDisposition::Protect
        } else if self.ignored_names.contains(process.name())
            || process
                .executable()
                .is_some_and(|path| self.ignored_executables.contains(path))
        {
            ProcessDisposition::Ignore
        } else {
            ProcessDisposition::Monitor
        }
    }

    #[must_use]
    pub fn is_protected_executable(&self, executable: &Path) -> bool {
        self.protected_executables.contains(executable)
    }

    pub fn ignore(&mut self, rule: IgnoreRule) {
        match rule {
            IgnoreRule::Name(name) => {
                self.ignored_names.insert(name);
            }
            IgnoreRule::Executable(path) => {
                self.ignored_executables.insert(path);
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct IgnoreRegistry {
    ignored_until: HashMap<ProcessIdentity, Duration>,
}

impl IgnoreRegistry {
    pub fn ignore_until(&mut self, identity: ProcessIdentity, deadline: Duration) {
        self.ignored_until.insert(identity, deadline);
    }

    #[must_use]
    pub fn is_ignored(&self, identity: ProcessIdentity, now: Duration) -> bool {
        self.ignored_until
            .get(&identity)
            .is_some_and(|deadline| now < *deadline)
    }

    pub fn remove_expired(&mut self, now: Duration) {
        self.ignored_until.retain(|_, deadline| now < *deadline);
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use super::{IgnoreRegistry, ProcessDisposition, ProtectionPolicy};
    use crate::domain::{ProcessDescriptor, ProcessIdentity};

    fn process(name: &str, executable: &str) -> ProcessDescriptor {
        ProcessDescriptor::new(
            ProcessIdentity::new(42, 1_000, 100),
            name,
            Some(PathBuf::from(executable)),
        )
    }

    #[test]
    fn protection_has_precedence_over_ignoring() {
        let policy = ProtectionPolicy::new(["shell".to_owned()], [], ["shell".to_owned()], []);

        assert_eq!(
            policy.disposition(&process("shell", "/bin/shell")),
            ProcessDisposition::Protect
        );
    }

    #[test]
    fn matches_ignored_executable() {
        let policy = ProtectionPolicy::new([], [], [], [PathBuf::from("/usr/bin/compiler")]);

        assert_eq!(
            policy.disposition(&process("worker", "/usr/bin/compiler")),
            ProcessDisposition::Ignore
        );
    }

    #[test]
    fn temporary_ignore_expires_at_its_deadline() {
        let identity = ProcessIdentity::new(42, 1_000, 100);
        let mut registry = IgnoreRegistry::default();
        registry.ignore_until(identity, Duration::from_secs(60));

        assert!(registry.is_ignored(identity, Duration::from_secs(59)));
        assert!(!registry.is_ignored(identity, Duration::from_secs(60)));

        registry.remove_expired(Duration::from_secs(60));
        assert!(!registry.is_ignored(identity, Duration::from_secs(59)));
    }

    #[test]
    fn temporary_ignore_does_not_follow_a_reused_pid() {
        let original = ProcessIdentity::new(42, 1_000, 100);
        let reused = ProcessIdentity::new(42, 1_000, 101);
        let mut registry = IgnoreRegistry::default();
        registry.ignore_until(original, Duration::from_secs(60));

        assert!(!registry.is_ignored(reused, Duration::from_secs(10)));
    }

    #[test]
    fn permanent_ignore_prefers_an_executable_path() {
        let worker = process("worker", "/usr/bin/worker");
        let mut policy = ProtectionPolicy::default();

        policy.ignore(super::IgnoreRule::for_process(&worker));

        assert_eq!(policy.disposition(&worker), ProcessDisposition::Ignore);
        assert_eq!(
            policy.disposition(&process("worker", "/opt/other-worker")),
            ProcessDisposition::Monitor
        );
    }
}
