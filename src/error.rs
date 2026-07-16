use std::{
    error::Error,
    fmt, io,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IoOperation {
    Enumerate,
    Read,
    Lock,
    CreateBackup,
    WriteTemporary,
    Replace,
    Verify,
}

#[derive(Debug)]
pub enum SteamError {
    Io {
        operation: IoOperation,
        path: PathBuf,
        source: io::Error,
        recovery_path: Option<PathBuf>,
    },
    InvalidVdf {
        path: Option<PathBuf>,
        reason: String,
    },
    MissingNode {
        path: Option<PathBuf>,
        node: String,
    },
    AmbiguousNode {
        path: Option<PathBuf>,
        node: String,
    },
    InvalidEdit {
        reason: &'static str,
    },
    InvalidPolicy {
        reason: &'static str,
    },
    SourceChanged {
        path: PathBuf,
        recovery_path: Option<PathBuf>,
    },
    UnsafeBackup {
        path: PathBuf,
    },
    BackupChanged {
        path: PathBuf,
        recovery_path: Option<PathBuf>,
    },
    BackupVerificationFailed {
        path: PathBuf,
    },
    WriteVerificationFailed {
        path: PathBuf,
        recovery_path: PathBuf,
    },
}

impl SteamError {
    pub(crate) fn io(operation: IoOperation, path: PathBuf, source: io::Error) -> Self {
        Self::Io {
            operation,
            path,
            source,
            recovery_path: None,
        }
    }

    pub(crate) fn read(path: PathBuf, source: io::Error) -> Self {
        Self::io(IoOperation::Read, path, source)
    }

    pub(crate) fn invalid_vdf(path: PathBuf, reason: impl Into<String>) -> Self {
        Self::InvalidVdf {
            path: Some(path),
            reason: reason.into(),
        }
    }

    pub(crate) fn invalid_vdf_source(reason: impl Into<String>) -> Self {
        Self::InvalidVdf {
            path: None,
            reason: reason.into(),
        }
    }

    pub(crate) fn invalid_vdf_at(path: Option<PathBuf>, reason: impl Into<String>) -> Self {
        Self::InvalidVdf {
            path,
            reason: reason.into(),
        }
    }

    pub(crate) fn missing_node(path: Option<PathBuf>, node: impl Into<String>) -> Self {
        Self::MissingNode {
            path,
            node: node.into(),
        }
    }

    pub(crate) fn ambiguous_node(path: Option<PathBuf>, node: impl Into<String>) -> Self {
        Self::AmbiguousNode {
            path,
            node: node.into(),
        }
    }

    pub(crate) fn invalid_edit(reason: &'static str) -> Self {
        Self::InvalidEdit { reason }
    }

    pub(crate) fn invalid_policy(reason: &'static str) -> Self {
        Self::InvalidPolicy { reason }
    }

    pub(crate) fn with_recovery(mut self, recovery_path: PathBuf) -> Self {
        match &mut self {
            Self::Io {
                recovery_path: current,
                ..
            }
            | Self::SourceChanged {
                recovery_path: current,
                ..
            }
            | Self::BackupChanged {
                recovery_path: current,
                ..
            } => *current = Some(recovery_path),
            _ => {}
        }
        self
    }

    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Io { path, .. } => Some(path),
            Self::InvalidVdf { path, .. }
            | Self::MissingNode { path, .. }
            | Self::AmbiguousNode { path, .. } => path.as_deref(),
            Self::SourceChanged { path, .. }
            | Self::UnsafeBackup { path }
            | Self::BackupChanged { path, .. }
            | Self::BackupVerificationFailed { path }
            | Self::WriteVerificationFailed { path, .. } => Some(path),
            Self::InvalidEdit { .. } | Self::InvalidPolicy { .. } => None,
        }
    }

    /// Returns the verified backup that can be used for recovery after a
    /// transaction failed beyond the backup boundary.
    pub fn recovery_path(&self) -> Option<&Path> {
        match self {
            Self::Io { recovery_path, .. }
            | Self::SourceChanged { recovery_path, .. }
            | Self::BackupChanged { recovery_path, .. } => recovery_path.as_deref(),
            Self::WriteVerificationFailed { recovery_path, .. } => Some(recovery_path),
            _ => None,
        }
    }
}

impl fmt::Display for SteamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
                ..
            } => write!(
                formatter,
                "Steam local {:?} failed for {}: {source}",
                operation,
                path.display()
            ),
            Self::InvalidVdf { path, reason } => {
                display_path_reason(formatter, "invalid Steam VDF", path.as_deref(), reason)
            }
            Self::MissingNode { path, node } => {
                display_path_reason(formatter, "missing Steam VDF node", path.as_deref(), node)
            }
            Self::AmbiguousNode { path, node } => {
                display_path_reason(formatter, "ambiguous Steam VDF node", path.as_deref(), node)
            }
            Self::InvalidEdit { reason } => write!(formatter, "invalid Steam edit: {reason}"),
            Self::InvalidPolicy { reason } => {
                write!(formatter, "invalid Steam transaction policy: {reason}")
            }
            Self::SourceChanged { path, .. } => {
                write!(formatter, "Steam source changed at {}", path.display())
            }
            Self::UnsafeBackup { path } => {
                write!(formatter, "unsafe Steam backup path: {}", path.display())
            }
            Self::BackupChanged { path, .. } => {
                write!(formatter, "Steam backup changed at {}", path.display())
            }
            Self::BackupVerificationFailed { path } => write!(
                formatter,
                "Steam backup verification failed at {}",
                path.display()
            ),
            Self::WriteVerificationFailed { path, .. } => write!(
                formatter,
                "Steam write verification failed at {}",
                path.display()
            ),
        }
    }
}

fn display_path_reason(
    formatter: &mut fmt::Formatter<'_>,
    message: &str,
    path: Option<&Path>,
    reason: &str,
) -> fmt::Result {
    if let Some(path) = path {
        write!(formatter, "{message} at {}: {reason}", path.display())
    } else {
        write!(formatter, "{message}: {reason}")
    }
}

impl Error for SteamError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::InvalidVdf { .. }
            | Self::MissingNode { .. }
            | Self::AmbiguousNode { .. }
            | Self::InvalidEdit { .. }
            | Self::InvalidPolicy { .. }
            | Self::SourceChanged { .. }
            | Self::UnsafeBackup { .. }
            | Self::BackupChanged { .. }
            | Self::BackupVerificationFailed { .. }
            | Self::WriteVerificationFailed { .. } => None,
        }
    }
}
