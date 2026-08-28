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

# Windows: the hardened NtCreateFile path gives us a non-reparse handle; MetadataExt exposes its
# stable volume/file index identity without reopening through a path-following API.
path = "codex-rs/exec-server/src/no_follow/windows.rs"
one(path, "use crate::regular_file;\n", "use crate::regular_file;\nuse super::DirectoryIdentity;\n")
one(
    path,
    "use std::os::windows::ffi::OsStrExt;\n",
    "use std::os::windows::ffi::OsStrExt;\nuse std::os::windows::fs::MetadataExt as _;\n",
)
one(
    path,
    "fn create_directory_sync(path: &Path, recursive: bool) -> io::Result<()> {\n",
    '''pub(super) async fn directory_identity(path: PathBuf) -> io::Result<DirectoryIdentity> {
    tokio::task::spawn_blocking(move || {
        let file = open_entry(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path is not a directory",
            ));
        }
        let first = metadata.volume_serial_number().ok_or_else(|| {
            io::Error::other("directory volume serial number is unavailable")
        })?;
        let second = metadata
            .file_index()
            .ok_or_else(|| io::Error::other("directory file index is unavailable"))?;
        Ok(DirectoryIdentity {
            first: u64::from(first),
            second,
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
    let path = native.into_path_buf();
    tokio::task::spawn_blocking(move || {
        let metadata = std::fs::metadata(&path)?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path is not a directory",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            return Ok(no_follow::DirectoryIdentity {
                first: metadata.dev(),
                second: metadata.ino(),
            });
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt as _;
            let first = metadata.volume_serial_number().ok_or_else(|| {
                io::Error::other("directory volume serial number is unavailable")
            })?;
            let second = metadata
                .file_index()
                .ok_or_else(|| io::Error::other("directory file index is unavailable"))?;
            return Ok(no_follow::DirectoryIdentity {
                first: u64::from(first),
                second,
            });
        }
        #[cfg(not(any(unix, windows)))]
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "stable directory identity is unavailable on this platform",
        ))
    })
    .await
    .map_err(|error| io::Error::other(format!("filesystem task failed: {error}")))?
}
'''
if text.count(old) != 1:
    raise RuntimeError(f"batch rollback directory block mismatch: {text.count(old)}")
text = text.replace(old, new, 1)
p.write_text(text)
