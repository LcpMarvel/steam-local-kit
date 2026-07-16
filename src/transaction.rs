use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions, Permissions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use fs2::FileExt;

use crate::{EditPlan, FileFingerprint, IoOperation, LocalConfigDocument, SteamError};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// Product-owned naming and safety policy for a write transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WritePolicy {
    /// Exact filename stem for the backup created by this transaction. A
    /// numeric suffix is added if that name already exists.
    pub backup_name_stem: String,
    /// Prefix accepted when opening an existing backup for restoration.
    pub allowed_backup_prefix: String,
    /// Re-read the source after backup creation and reject concurrent changes.
    ///
    /// Transactions using this crate are also serialized with a per-target
    /// advisory lock held across the final source check and atomic replace.
    pub require_unchanged_source: bool,
    /// Apply the target file's permissions to backups and replacement files.
    pub preserve_permissions: bool,
}

impl WritePolicy {
    pub fn new(
        backup_name_stem: impl Into<String>,
        allowed_backup_prefix: impl Into<String>,
    ) -> Self {
        Self {
            backup_name_stem: backup_name_stem.into(),
            allowed_backup_prefix: allowed_backup_prefix.into(),
            require_unchanged_source: true,
            preserve_permissions: true,
        }
    }

    fn validate(&self) -> Result<(), SteamError> {
        validate_file_component(
            &self.backup_name_stem,
            "backup_name_stem_must_be_one_file_component",
        )?;
        validate_file_component(
            &self.allowed_backup_prefix,
            "allowed_backup_prefix_must_be_one_file_component",
        )?;
        if !self
            .backup_name_stem
            .starts_with(&self.allowed_backup_prefix)
        {
            return Err(SteamError::invalid_policy(
                "backup_name_stem_must_start_with_allowed_prefix",
            ));
        }
        Ok(())
    }
}

/// An existing, same-directory VDF backup whose bytes were fingerprinted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedBackup {
    target_path: PathBuf,
    path: PathBuf,
    fingerprint: FileFingerprint,
}

impl VerifiedBackup {
    pub fn target_path(&self) -> &Path {
        &self.target_path
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn fingerprint(&self) -> &FileFingerprint {
        &self.fingerprint
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplyResult {
    pub path: PathBuf,
    pub changed: bool,
    pub backup: Option<VerifiedBackup>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestoreResult {
    pub path: PathBuf,
    pub restored_from: PathBuf,
    pub protection_backup: VerifiedBackup,
}

pub struct SafeTransaction;

impl SafeTransaction {
    /// Applies a lossless edit plan after binding it to the current source,
    /// creating and verifying a backup, and checking for concurrent changes.
    pub fn apply(
        path: &Path,
        plan: &EditPlan,
        policy: &WritePolicy,
    ) -> Result<ApplyResult, SteamError> {
        policy.validate()?;
        validate_plan(path, plan)?;

        let current = read_bytes(path)?;
        let current_fingerprint = FileFingerprint::from_bytes(&current);
        if &current_fingerprint != plan.source_fingerprint() {
            return Err(SteamError::SourceChanged {
                path: path.to_path_buf(),
                recovery_path: None,
            });
        }
        if !plan.changed() {
            return Ok(ApplyResult {
                path: path.to_path_buf(),
                changed: false,
                backup: None,
            });
        }

        let _transaction_lock = TargetTransactionLock::acquire(path)?;
        let locked_current = read_bytes(path)?;
        if locked_current != current {
            return Err(SteamError::SourceChanged {
                path: path.to_path_buf(),
                recovery_path: None,
            });
        }

        let permissions = target_permissions(path, policy)?;
        let backup = create_verified_backup(path, &locked_current, permissions.as_ref(), policy)?;
        if policy.require_unchanged_source && read_bytes(path)? != locked_current {
            return Err(SteamError::SourceChanged {
                path: path.to_path_buf(),
                recovery_path: Some(backup.path.clone()),
            });
        }

        atomic_replace(path, plan.replacement(), permissions.as_ref())
            .map_err(|error| error.with_recovery(backup.path.clone()))?;
        let written = fs::read(path).map_err(|source| {
            SteamError::io(IoOperation::Verify, path.to_path_buf(), source)
                .with_recovery(backup.path.clone())
        })?;
        if written != plan.replacement() {
            return Err(SteamError::WriteVerificationFailed {
                path: path.to_path_buf(),
                recovery_path: backup.path.clone(),
            });
        }

        Ok(ApplyResult {
            path: path.to_path_buf(),
            changed: true,
            backup: Some(backup),
        })
    }

    /// Opens only a regular, same-directory VDF file with the caller's backup
    /// prefix and binds the verification to one target path.
    pub fn verify_backup(
        target_path: &Path,
        backup_path: &Path,
        allowed_backup_prefix: &str,
    ) -> Result<VerifiedBackup, SteamError> {
        validate_file_component(
            allowed_backup_prefix,
            "allowed_backup_prefix_must_be_one_file_component",
        )?;
        let target_parent = target_path
            .parent()
            .ok_or_else(|| SteamError::invalid_policy("target_parent_missing"))?;
        let safe_name = backup_path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.starts_with(allowed_backup_prefix));
        if backup_path.parent() != Some(target_parent) || !safe_name {
            return Err(SteamError::UnsafeBackup {
                path: backup_path.to_path_buf(),
            });
        }
        let metadata = fs::symlink_metadata(backup_path)
            .map_err(|source| SteamError::read(backup_path.to_path_buf(), source))?;
        if !metadata.file_type().is_file() {
            return Err(SteamError::UnsafeBackup {
                path: backup_path.to_path_buf(),
            });
        }
        let bytes = read_bytes(backup_path)?;
        validate_vdf_bytes(backup_path, &bytes)?;
        Ok(VerifiedBackup {
            target_path: target_path.to_path_buf(),
            path: backup_path.to_path_buf(),
            fingerprint: FileFingerprint::from_bytes(&bytes),
        })
    }

    /// Restores a verified backup while first preserving the current target in
    /// a new, independently verified protection backup.
    pub fn restore(
        path: &Path,
        backup: &VerifiedBackup,
        policy: &WritePolicy,
    ) -> Result<RestoreResult, SteamError> {
        policy.validate()?;
        if backup.target_path != path {
            return Err(SteamError::UnsafeBackup {
                path: backup.path.clone(),
            });
        }
        let safe_name = backup
            .path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.starts_with(&policy.allowed_backup_prefix));
        if backup.path.parent() != path.parent() || !safe_name {
            return Err(SteamError::UnsafeBackup {
                path: backup.path.clone(),
            });
        }

        let replacement = read_bytes(&backup.path)?;
        if FileFingerprint::from_bytes(&replacement) != backup.fingerprint {
            return Err(SteamError::BackupChanged {
                path: backup.path.clone(),
                recovery_path: None,
            });
        }
        validate_vdf_bytes(&backup.path, &replacement)?;

        let _transaction_lock = TargetTransactionLock::acquire(path)?;
        let replacement_locked = read_bytes(&backup.path)?;
        if FileFingerprint::from_bytes(&replacement_locked) != backup.fingerprint {
            return Err(SteamError::BackupChanged {
                path: backup.path.clone(),
                recovery_path: None,
            });
        }

        let current = read_bytes(path)?;
        let permissions = target_permissions(path, policy)?;
        let protection = create_verified_backup(path, &current, permissions.as_ref(), policy)?;
        if policy.require_unchanged_source && read_bytes(path)? != current {
            return Err(SteamError::SourceChanged {
                path: path.to_path_buf(),
                recovery_path: Some(protection.path.clone()),
            });
        }
        let replacement_now = read_bytes(&backup.path)?;
        if FileFingerprint::from_bytes(&replacement_now) != backup.fingerprint {
            return Err(SteamError::BackupChanged {
                path: backup.path.clone(),
                recovery_path: Some(protection.path.clone()),
            });
        }

        atomic_replace(path, &replacement_now, permissions.as_ref())
            .map_err(|error| error.with_recovery(protection.path.clone()))?;
        let restored = fs::read(path).map_err(|source| {
            SteamError::io(IoOperation::Verify, path.to_path_buf(), source)
                .with_recovery(protection.path.clone())
        })?;
        if restored != replacement_now {
            return Err(SteamError::WriteVerificationFailed {
                path: path.to_path_buf(),
                recovery_path: protection.path.clone(),
            });
        }

        Ok(RestoreResult {
            path: path.to_path_buf(),
            restored_from: backup.path.clone(),
            protection_backup: protection,
        })
    }
}

fn validate_plan(path: &Path, plan: &EditPlan) -> Result<(), SteamError> {
    let replacement_fingerprint = FileFingerprint::from_bytes(plan.replacement());
    if plan.changed() {
        if plan.changes().is_empty() || &replacement_fingerprint == plan.source_fingerprint() {
            return Err(SteamError::invalid_edit("inconsistent_changed_plan"));
        }
    } else if !plan.changes().is_empty() || &replacement_fingerprint != plan.source_fingerprint() {
        return Err(SteamError::invalid_edit("inconsistent_unchanged_plan"));
    }
    validate_vdf_bytes(path, plan.replacement())
}

fn validate_vdf_bytes(path: &Path, bytes: &[u8]) -> Result<(), SteamError> {
    let source = std::str::from_utf8(bytes)
        .map_err(|_| SteamError::invalid_vdf(path.to_path_buf(), "source_is_not_utf8"))?;
    LocalConfigDocument::parse_at(path, source).map(|_| ())
}

fn validate_file_component(value: &str, reason: &'static str) -> Result<(), SteamError> {
    let path = Path::new(value);
    if value.is_empty()
        || path.file_name() != Some(OsStr::new(value))
        || path.components().count() != 1
    {
        Err(SteamError::invalid_policy(reason))
    } else {
        Ok(())
    }
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, SteamError> {
    fs::read(path).map_err(|source| SteamError::read(path.to_path_buf(), source))
}

struct TargetTransactionLock {
    _lock_file: File,
    #[cfg(unix)]
    _target_file: File,
}

impl TargetTransactionLock {
    fn acquire(path: &Path) -> Result<Self, SteamError> {
        let lock_path = transaction_lock_path(path)?;
        let lock_file = open_lock_file(&lock_path)
            .map_err(|source| SteamError::io(IoOperation::Lock, lock_path.clone(), source))?;
        lock_file
            .try_lock_exclusive()
            .map_err(|source| SteamError::io(IoOperation::Lock, lock_path, source))?;

        #[cfg(unix)]
        let target_file = {
            let target_file = File::open(path)
                .map_err(|source| SteamError::io(IoOperation::Lock, path.to_path_buf(), source))?;
            target_file
                .try_lock_exclusive()
                .map_err(|source| SteamError::io(IoOperation::Lock, path.to_path_buf(), source))?;
            target_file
        };

        Ok(Self {
            _lock_file: lock_file,
            #[cfg(unix)]
            _target_file: target_file,
        })
    }
}

impl Drop for TargetTransactionLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = FileExt::unlock(&self._target_file);
        let _ = FileExt::unlock(&self._lock_file);
    }
}

fn transaction_lock_path(path: &Path) -> Result<PathBuf, SteamError> {
    let parent = path
        .parent()
        .ok_or_else(|| SteamError::invalid_policy("target_parent_missing"))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| SteamError::invalid_policy("target_file_name_missing"))?;
    let mut lock_name = OsString::from(".");
    lock_name.push(file_name);
    lock_name.push(".steam-local-kit.lock");
    Ok(parent.join(lock_name))
}

fn open_lock_file(path: &Path) -> io::Result<File> {
    let mut create_options = OpenOptions::new();
    create_options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        create_options.mode(0o600);
    }

    match create_options.open(path) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)?;
            if !metadata.file_type().is_file() {
                return Err(io::Error::other(
                    "Steam transaction lock path is not a regular file",
                ));
            }
            OpenOptions::new().read(true).write(true).open(path)
        }
        Err(error) => Err(error),
    }
}

fn target_permissions(
    path: &Path,
    policy: &WritePolicy,
) -> Result<Option<Permissions>, SteamError> {
    if policy.preserve_permissions {
        fs::metadata(path)
            .map(|metadata| Some(metadata.permissions()))
            .map_err(|source| SteamError::read(path.to_path_buf(), source))
    } else {
        Ok(None)
    }
}

fn create_verified_backup(
    target: &Path,
    source_bytes: &[u8],
    permissions: Option<&Permissions>,
    policy: &WritePolicy,
) -> Result<VerifiedBackup, SteamError> {
    let parent = target
        .parent()
        .ok_or_else(|| SteamError::invalid_policy("target_parent_missing"))?;
    let (path, mut file) = create_unique_file(parent, &policy.backup_name_stem, permissions)?;
    let write_result = (|| -> io::Result<()> {
        file.write_all(source_bytes)?;
        file.sync_all()?;
        drop(file);
        Ok(())
    })();
    if let Err(source) = write_result {
        let _ = fs::remove_file(&path);
        return Err(SteamError::io(IoOperation::CreateBackup, path, source));
    }
    let verified = match fs::read(&path) {
        Ok(verified) => verified,
        Err(source) => {
            let _ = fs::remove_file(&path);
            return Err(SteamError::io(IoOperation::Verify, path, source));
        }
    };
    if verified != source_bytes {
        let _ = fs::remove_file(&path);
        return Err(SteamError::BackupVerificationFailed { path });
    }
    Ok(VerifiedBackup {
        target_path: target.to_path_buf(),
        path,
        fingerprint: FileFingerprint::from_bytes(source_bytes),
    })
}

fn create_unique_file(
    parent: &Path,
    stem: &str,
    permissions: Option<&Permissions>,
) -> Result<(PathBuf, File), SteamError> {
    for collision in 0_u32.. {
        let name = if collision == 0 {
            stem.to_owned()
        } else {
            format!("{stem}-{collision}")
        };
        let path = parent.join(name);
        match create_sensitive_file(&path, permissions) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(SteamError::io(IoOperation::CreateBackup, path, source));
            }
        }
    }
    unreachable!("u32 backup collision space exhausted")
}

fn create_sensitive_file(path: &Path, permissions: Option<&Permissions>) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let file = options.open(path)?;
    if let Some(permissions) = permissions {
        if let Err(error) = file.set_permissions(permissions.clone()) {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(error);
        }
    }
    Ok(file)
}

fn atomic_replace(
    path: &Path,
    replacement: &[u8],
    permissions: Option<&Permissions>,
) -> Result<(), SteamError> {
    let parent = path
        .parent()
        .ok_or_else(|| SteamError::invalid_policy("target_parent_missing"))?;
    let temp_id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let mut attempt = 0_u32;
    let (temp_path, mut file) = loop {
        let candidate = parent.join(format!(
            ".steam-local-kit-{}-{temp_id}-{attempt}.tmp",
            std::process::id()
        ));
        match create_sensitive_file(&candidate, permissions) {
            Ok(file) => break (candidate, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                attempt = attempt
                    .checked_add(1)
                    .ok_or_else(|| SteamError::invalid_policy("temporary_name_space_exhausted"))?;
            }
            Err(source) => {
                return Err(SteamError::io(
                    IoOperation::WriteTemporary,
                    candidate,
                    source,
                ));
            }
        }
    };

    let write_result = (|| -> io::Result<()> {
        file.write_all(replacement)?;
        file.sync_all()?;
        drop(file);
        Ok(())
    })();
    if let Err(source) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(SteamError::io(
            IoOperation::WriteTemporary,
            temp_path,
            source,
        ));
    }

    if let Err(source) = replace_file(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(SteamError::io(
            IoOperation::Replace,
            path.to_path_buf(),
            source,
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::Storage::FileSystem::{ReplaceFileW, REPLACEFILE_WRITE_THROUGH};

    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    let replaced = unsafe {
        ReplaceFileW(
            to.as_ptr(),
            from.as_ptr(),
            ptr::null(),
            REPLACEFILE_WRITE_THROUGH,
            ptr::null(),
            ptr::null(),
        )
    };
    if replaced == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::{AppId, LaunchOptionsEdit};

    use super::*;

    const SAMPLE: &str = r#""UserLocalConfigStore"
{
    "Software" { "Valve" { "Steam" { "apps" { "570"
    {
        "LastPlayed" "1"
    }
    } } } }
}
"#;

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "steam-local-kit-transaction-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn edit_plan(source: &str) -> EditPlan {
        LocalConfigDocument::parse(source)
            .unwrap()
            .plan_launch_options_edit(
                AppId::new(570),
                LaunchOptionsEdit::EnsureToken("-gamestateintegration"),
            )
            .unwrap()
    }

    fn policy(stem: &str) -> WritePolicy {
        WritePolicy::new(stem, "localconfig.vdf.back-")
    }

    #[test]
    fn apply_creates_verified_backup_and_exact_replacement() {
        let root = temp_root("apply");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("localconfig.vdf");
        fs::write(&target, SAMPLE).unwrap();
        let plan = edit_plan(SAMPLE);

        let result =
            SafeTransaction::apply(&target, &plan, &policy("localconfig.vdf.back-test")).unwrap();

        assert!(result.changed);
        let backup = result.backup.unwrap();
        assert_eq!(fs::read(backup.path()).unwrap(), SAMPLE.as_bytes());
        assert_eq!(fs::read(&target).unwrap(), plan.replacement());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unchanged_plan_creates_no_backup() {
        let source = SAMPLE.replace(
            "\"LastPlayed\" \"1\"",
            "\"LaunchOptions\" \"-gamestateintegration\"",
        );
        let root = temp_root("unchanged");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("localconfig.vdf");
        fs::write(&target, &source).unwrap();
        let plan = edit_plan(&source);

        let result =
            SafeTransaction::apply(&target, &plan, &policy("localconfig.vdf.back-test")).unwrap();

        assert!(!result.changed);
        assert!(result.backup.is_none());
        assert_eq!(fs::read_to_string(&target).unwrap(), source);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_plan_is_rejected_before_backup() {
        let root = temp_root("stale");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("localconfig.vdf");
        fs::write(&target, SAMPLE).unwrap();
        let plan = edit_plan(SAMPLE);
        let changed = SAMPLE.replace("\"LastPlayed\" \"1\"", "\"LastPlayed\" \"2\"");
        fs::write(&target, &changed).unwrap();

        let error = SafeTransaction::apply(&target, &plan, &policy("localconfig.vdf.back-test"))
            .unwrap_err();

        assert!(matches!(error, SteamError::SourceChanged { .. }));
        assert!(error.recovery_path().is_none());
        assert_eq!(fs::read_to_string(&target).unwrap(), changed);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn backup_name_collision_gets_a_numeric_suffix() {
        let root = temp_root("collision");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("localconfig.vdf");
        let occupied = root.join("localconfig.vdf.back-test");
        fs::write(&target, SAMPLE).unwrap();
        fs::write(&occupied, "do-not-overwrite").unwrap();
        let plan = edit_plan(SAMPLE);

        let result =
            SafeTransaction::apply(&target, &plan, &policy("localconfig.vdf.back-test")).unwrap();

        assert_eq!(
            result.backup.unwrap().path(),
            root.join("localconfig.vdf.back-test-1")
        );
        assert_eq!(fs::read_to_string(occupied).unwrap(), "do-not-overwrite");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restore_requires_same_directory_prefix_and_preserves_current_bytes() {
        let root = temp_root("restore");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("localconfig.vdf");
        let original = root.join("localconfig.vdf.back-original");
        let current = String::from_utf8(edit_plan(SAMPLE).into_replacement()).unwrap();
        fs::write(&target, &current).unwrap();
        fs::write(&original, SAMPLE).unwrap();
        let verified =
            SafeTransaction::verify_backup(&target, &original, "localconfig.vdf.back-").unwrap();

        let result = SafeTransaction::restore(
            &target,
            &verified,
            &policy("localconfig.vdf.back-protection"),
        )
        .unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), SAMPLE);
        assert_eq!(
            fs::read_to_string(result.protection_backup.path()).unwrap(),
            current
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn backup_outside_target_directory_is_rejected() {
        let root = temp_root("unsafe");
        let outside = temp_root("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let target = root.join("localconfig.vdf");
        let backup = outside.join("localconfig.vdf.back-test");
        fs::write(&target, SAMPLE).unwrap();
        fs::write(&backup, SAMPLE).unwrap();

        let error =
            SafeTransaction::verify_backup(&target, &backup, "localconfig.vdf.back-").unwrap_err();

        assert!(matches!(error, SteamError::UnsafeBackup { .. }));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn invalid_backup_error_does_not_echo_file_contents() {
        let root = temp_root("invalid-backup");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("localconfig.vdf");
        let backup = root.join("localconfig.vdf.back-test");
        fs::write(&target, SAMPLE).unwrap();
        fs::write(&backup, "secret-token {").unwrap();

        let error =
            SafeTransaction::verify_backup(&target, &backup, "localconfig.vdf.back-").unwrap_err();

        assert!(matches!(error, SteamError::InvalidVdf { .. }));
        assert!(!error.to_string().contains("secret-token"));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn apply_preserves_target_permissions_on_replacement_and_backup() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("permissions");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("localconfig.vdf");
        fs::write(&target, SAMPLE).unwrap();
        fs::set_permissions(&target, Permissions::from_mode(0o640)).unwrap();
        let plan = edit_plan(SAMPLE);

        let result =
            SafeTransaction::apply(&target, &plan, &policy("localconfig.vdf.back-test")).unwrap();
        let backup = result.backup.unwrap();

        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(
            fs::metadata(backup.path()).unwrap().permissions().mode() & 0o777,
            0o640
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn sensitive_file_permissions_are_applied_before_any_content_is_written() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("prewrite-permissions");
        fs::create_dir_all(&root).unwrap();
        let inherited = Permissions::from_mode(0o640);

        let (_path, file) =
            create_unique_file(&root, "localconfig.vdf.back-test", Some(&inherited)).unwrap();

        assert_eq!(file.metadata().unwrap().len(), 0);
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o640);
        drop(file);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn sensitive_files_default_to_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("secure-default-permissions");
        fs::create_dir_all(&root).unwrap();

        let (path, file) = create_unique_file(&root, "localconfig.vdf.back-test", None).unwrap();

        assert_eq!(file.metadata().unwrap().len(), 0);
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o077, 0);
        drop(file);
        assert!(path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_transactions_fail_closed_while_the_target_is_locked() {
        let root = temp_root("transaction-lock");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("localconfig.vdf");
        fs::write(&target, SAMPLE).unwrap();
        let plan = edit_plan(SAMPLE);
        let held_lock = TargetTransactionLock::acquire(&target).unwrap();

        let error = SafeTransaction::apply(&target, &plan, &policy("localconfig.vdf.back-test"))
            .unwrap_err();

        assert!(matches!(
            error,
            SteamError::Io {
                operation: IoOperation::Lock,
                ..
            }
        ));
        assert_eq!(fs::read_to_string(&target).unwrap(), SAMPLE);
        assert!(!root.join("localconfig.vdf.back-test").exists());
        drop(held_lock);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changed_backup_is_rejected_after_verification() {
        let root = temp_root("backup-changed");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("localconfig.vdf");
        let backup = root.join("localconfig.vdf.back-test");
        fs::write(&target, SAMPLE).unwrap();
        fs::write(&backup, SAMPLE).unwrap();
        let verified =
            SafeTransaction::verify_backup(&target, &backup, "localconfig.vdf.back-").unwrap();
        fs::write(
            &backup,
            SAMPLE.replace("\"LastPlayed\" \"1\"", "\"LastPlayed\" \"2\""),
        )
        .unwrap();

        let error = SafeTransaction::restore(
            &target,
            &verified,
            &policy("localconfig.vdf.back-protection"),
        )
        .unwrap_err();

        assert!(matches!(error, SteamError::BackupChanged { .. }));
        assert!(error.recovery_path().is_none());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn policy_rejects_path_components() {
        let root = temp_root("policy");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("localconfig.vdf");
        fs::write(&target, SAMPLE).unwrap();
        let plan = edit_plan(SAMPLE);

        let error = SafeTransaction::apply(&target, &plan, &WritePolicy::new("../outside", ".."))
            .unwrap_err();

        assert!(matches!(error, SteamError::InvalidPolicy { .. }));
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }
}
