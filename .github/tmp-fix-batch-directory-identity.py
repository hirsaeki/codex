from pathlib import Path


def one(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    if text.count(old) != 1:
        raise RuntimeError(f"{path}: expected one match, got {text.count(old)} for {old[:80]!r}")
    p.write_text(text.replace(old, new, 1))


# Expose a no-follow directory identity derived from the already-hardened open path.
path = "codex-rs/exec-server/src/no_follow/mod.rs"
one(
    path,
    "use std::path::Path;\n",
    "use std::path::Path;\n\n#[derive(Clone, Copy, Debug, Eq, PartialEq)]\npub(crate) struct DirectoryIdentity {\n    pub(crate) first: u64,\n    pub(crate) second: u64,\n}\n",
)
one(
    path,
    "pub(crate) async fn create_directory(path: &Path, recursive: bool) -> io::Result<()> {\n",
    "pub(crate) async fn directory_identity(path: &Path) -> io::Result<DirectoryIdentity> {\n    imp::directory_identity(path.to_path_buf()).await\n}\n\npub(crate) async fn create_directory(path: &Path, recursive: bool) -> io::Result<()> {\n",
)

# Unix: derive identity from the descriptor returned by the hardened no-follow opener.
path = "codex-rs/exec-server/src/no_follow/unix.rs"
one(path, "use crate::FileMetadata;\n", "use crate::FileMetadata;\nuse super::DirectoryIdentity;\n")
one(
    path,
    "use std::os::fd::OwnedFd;\n",
    "use std::os::fd::OwnedFd;\nuse std::os::unix::fs::MetadataExt as _;\n",
)
one(
    path,
    "pub(super) async fn create_directory(path: PathBuf, recursive: bool) -> io::Result<()> {\n",
    '''pub(super) async fn directory_identity(path: PathBuf) -> io::Result<DirectoryIdentity> {
    tokio::task::spawn_blocking(move || {
        let file = open_entry_sync(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path is not a directory",
            ));
        }
        Ok(DirectoryIdentity {
            first: metadata.dev(),
            second: metadata.ino(),
        })
    })
    .await
    .map_err(|error| io::Error::other(format!("filesystem task failed: {error}")))?
}

pub(super) async fn create_directory(path: PathBuf, recursive: bool) -> io::Result<()> {
''',
)

# Windows: use the existing hardened NtCreateFile path, then query identity from that exact handle.
path = "codex-rs/exec-server/src/no_follow/windows.rs"
one(path, "use crate::regular_file;\n", "use crate::regular_file;\nuse super::DirectoryIdentity;\n")
one(
    path,
    "use windows_sys::Win32::Storage::FileSystem::DELETE;\n",
    "use windows_sys::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION;\nuse windows_sys::Win32::Storage::FileSystem::DELETE;\n",
)
one(
    path,
    "use windows_sys::Win32::Storage::FileSystem::FileDispositionInfo;\n",
    "use windows_sys::Win32::Storage::FileSystem::FileDispositionInfo;\nuse windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandle;\n",
)
one(
    path,
    "fn create_directory_sync(path: &Path, recursive: bool) -> io::Result<()> {\n",
    '''pub(super) async fn directory_identity(path: PathBuf) -> io::Result<DirectoryIdentity> {
    tokio::task::spawn_blocking(move || {
        let file = open_entry(&path)?;
        if !file.metadata()?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path is not a directory",
            ));
        }
        let mut info = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
        let result = unsafe {
            GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info)
        };
        if result == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(DirectoryIdentity {
            first: u64::from(info.dwVolumeSerialNumber),
            second: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        })
    })
    .await
    .map_err(|error| io::Error::other(format!("filesystem task failed: {error}")))?
}

fn create_directory_sync(path: &Path, recursive: bool) -> io::Result<()> {
''',
)

# Batch engine: created-directory rollback uses stable identity rather than mutable timestamps.
path = "codex-rs/exec-server/src/file_mutation_batch.rs"
p = Path(path)
text = p.read_text()
text = text.replace("use crate::FileMetadata;\n", "", 1)
text = text.replace("use crate::local_file_system::DirectFileSystem;\n", "use crate::local_file_system::DirectFileSystem;\nuse crate::no_follow;\n", 1)
text = text.replace(
    "    metadata: FileMetadata,\n",
    "    identity: no_follow::DirectoryIdentity,\n",
    1,
)
text = text.replace(
    '''                created.push(CreatedDirectory {
                    path: path_uri,
                    metadata,
                });
''',
    '''                let identity = directory_identity(&path_uri, follow_symlinks).await?;
                created.push(CreatedDirectory {
                    path: path_uri,
                    identity,
                });
''',
    1,
)
old = '''async fn rollback_created_directory(
    file_system: &dyn ExecutorFileSystem,
    directory: &CreatedDirectory,
    follow_symlinks: bool,
) -> io::Result<()> {
    let metadata = match file_system
        .get_metadata(
            &directory.path,
            GetMetadataOptions { follow_symlinks },
            /*sandbox*/ None,
        )
        .await
    {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata != directory.metadata || metadata.is_symlink || !metadata.is_directory {
        return Err(io::Error::other(
            "created directory changed before rollback; refusing to remove it",
        ));
    }
    file_system
        .remove(
            &directory.path,
            RemoveOptions {
                recursive: false,
                force: false,
                follow_symlinks,
            },
            /*sandbox*/ None,
        )
        .await?;
    match file_system
        .get_metadata(
            &directory.path,
            GetMetadataOptions { follow_symlinks },
            /*sandbox*/ None,
        )
        .await
    {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(io::Error::other("created directory still exists after rollback")),
        Err(error) => Err(error),
    }
}
'''
new = '''async fn rollback_created_directory(
    file_system: &dyn ExecutorFileSystem,
    directory: &CreatedDirectory,
    follow_symlinks: bool,
) -> io::Result<()> {
    let identity = match directory_identity(&directory.path, follow_symlinks).await {
        Ok(identity) => identity,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if identity != directory.identity {
        return Err(io::Error::other(
            "created directory identity changed before rollback; refusing to remove it",
        ));
    }
    // Non-recursive removal is intentional: if another actor added content, removal fails rather
    // than deleting data that the batch did not create.
    file_system
        .remove(
            &directory.path,
            RemoveOptions {
                recursive: false,
                force: false,
                follow_symlinks,
            },
            /*sandbox*/ None,
        )
        .await?;
    match directory_identity(&directory.path, follow_symlinks).await {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(io::Error::other("created directory still exists after rollback")),
        Err(error) => Err(error),
    }
}

async fn directory_identity(
    path: &PathUri,
    follow_symlinks: bool,
) -> io::Result<no_follow::DirectoryIdentity> {
    let native = path.to_abs_path()?;
    if !follow_symlinks {
        return no_follow::directory_identity(native.as_path()).await;
    }
    // Following is explicitly enabled for this batch. Canonicalize first, then obtain identity
    // through the no-follow primitive for the resolved target.
    let canonicalized = tokio::fs::canonicalize(native.as_path()).await?;
    no_follow::directory_identity(canonicalized.as_path()).await
}
'''
if text.count(old) != 1:
    raise RuntimeError(f"batch rollback directory block mismatch: {text.count(old)}")
text = text.replace(old, new, 1)
p.write_text(text)
