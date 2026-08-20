use super::CreatedDirectory;
use super::FileIdentity;
use super::Snapshot;
use super::path_identity;
use super::read_snapshot;
use super::validate_parent;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use uuid::Uuid;

pub(super) struct QuarantinedFile {
    path: PathBuf,
    snapshot: Option<Snapshot>,
    restore_on_rollback: bool,
}

impl QuarantinedFile {
    pub(super) fn removed(path: PathBuf, snapshot: Option<Snapshot>) -> Self {
        Self {
            path,
            snapshot,
            restore_on_rollback: true,
        }
    }

    pub(super) fn staged(path: PathBuf, snapshot: Option<Snapshot>) -> Self {
        Self {
            path,
            snapshot,
            restore_on_rollback: false,
        }
    }

    pub(super) fn path(&self) -> &Path {
        self.path.as_path()
    }

    pub(super) fn snapshot(&self) -> Option<&Snapshot> {
        self.snapshot.as_ref()
    }

    pub(super) fn restore_on_rollback(&self) -> bool {
        self.restore_on_rollback
    }
}

pub(super) fn private_path(path: &Path) -> io::Result<PathBuf> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory")
    })?;
    for _ in 0..16 {
        let candidate = parent.join(format!(".codex-quarantine-{}", Uuid::new_v4()));
        match std::fs::symlink_metadata(candidate.as_path()) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(candidate);
            }
            Ok(_) => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a private quarantine path",
    ))
}

pub(super) fn quarantine_path(path: &Path) -> io::Result<PathBuf> {
    let candidate = private_path(path)?;
    rename_no_replace(path, candidate.as_path())?;
    Ok(candidate)
}

pub(super) fn publish_staged_file(staged_path: &Path, path: &Path) -> io::Result<()> {
    rename_no_replace(staged_path, path)
}

pub(super) fn restore_quarantined_file(
    path: &Path,
    quarantine_path: &Path,
    expected: &Snapshot,
    before_rename: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    validate_parent(path)?;
    if read_snapshot(quarantine_path)? != *expected || read_snapshot(path)? != Snapshot::Missing {
        return Err(io::Error::other("quarantined file changed before restore"));
    }
    before_rename()?;
    rename_no_replace(quarantine_path, path)?;
    if read_snapshot(path)? != *expected || read_snapshot(quarantine_path)? != Snapshot::Missing {
        return Err(io::Error::other("path changed during quarantine restore"));
    }
    Ok(())
}

pub(super) fn discard_quarantined_file(path: &Path, expected: &Snapshot) -> io::Result<()> {
    validate_parent(path)?;
    if read_snapshot(path)? != *expected {
        return Err(io::Error::other("quarantined file changed before cleanup"));
    }
    let private_path = quarantine_path(path)?;
    if read_snapshot(private_path.as_path())? != *expected {
        return Err(io::Error::other("quarantined file changed during cleanup"));
    }
    #[cfg(windows)]
    {
        let mut permissions = std::fs::metadata(private_path.as_path())?.permissions();
        if permissions.readonly() {
            permissions.set_readonly(false);
            std::fs::set_permissions(private_path.as_path(), permissions)?;
        }
    }
    std::fs::remove_file(private_path.as_path())?;
    if read_snapshot(private_path.as_path())? != Snapshot::Missing {
        return Err(io::Error::other("quarantined file remained after cleanup"));
    }
    Ok(())
}

fn restore_quarantined_directory(
    path: &Path,
    quarantine_path: &Path,
    expected: FileIdentity,
) -> io::Result<()> {
    if directory_identity(quarantine_path)? != Some(expected) || directory_identity(path)?.is_some()
    {
        return Err(io::Error::other(
            "quarantined directory changed before restore",
        ));
    }
    rename_no_replace(quarantine_path, path)?;
    if directory_identity(path)? != Some(expected) || directory_identity(quarantine_path)?.is_some()
    {
        return Err(io::Error::other(
            "directory changed during quarantine restore",
        ));
    }
    Ok(())
}

pub(super) fn remove_created_directory(
    directory: &CreatedDirectory,
    before_rename: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let expected = directory.identity.ok_or_else(|| {
        io::Error::other("created directory identity is unavailable during rollback")
    })?;
    if directory_identity(directory.path.as_path())? != Some(expected) {
        return Err(io::Error::other(
            "created directory identity changed before rollback",
        ));
    }
    before_rename()?;
    let quarantine_path = quarantine_path(directory.path.as_path())?;
    let moved = directory_identity(quarantine_path.as_path());
    if moved.as_ref().ok() != Some(&Some(expected)) {
        if let Ok(Some(moved)) = moved
            && directory_identity(directory.path.as_path())?.is_none()
        {
            let _ = restore_quarantined_directory(
                directory.path.as_path(),
                quarantine_path.as_path(),
                moved,
            );
        }
        return Err(io::Error::other(
            "created directory identity changed during rollback",
        ));
    }
    std::fs::remove_dir(quarantine_path.as_path())?;
    if directory_identity(directory.path.as_path())?.is_some() {
        return Err(io::Error::other(
            "created directory path changed during rollback",
        ));
    }
    Ok(())
}

pub(super) fn directory_identity(path: &Path) -> io::Result<Option<FileIdentity>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            path_identity(path, &metadata)
        }
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path `{}` is not a directory", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let from = CString::new(from.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source path contains NUL"))?;
    let to = CString::new(to.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "destination path contains NUL")
    })?;
    // SAFETY: both paths are NUL-terminated and remain alive for the syscall.
    if unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let from = CString::new(from.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source path contains NUL"))?;
    let to = CString::new(to.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "destination path contains NUL")
    })?;
    // SAFETY: both paths are NUL-terminated and remain alive for the call.
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;

    let from: Vec<u16> = from.as_os_str().encode_wide().chain(once(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(once(0)).collect();
    // SAFETY: both paths are NUL-terminated and remain alive for the call. Omitting
    // MOVEFILE_REPLACE_EXISTING gives this operation no-overwrite semantics.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    windows
)))]
fn rename_no_replace(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic no-overwrite rename is unavailable on this platform",
    ))
}
