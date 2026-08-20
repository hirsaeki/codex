use codex_file_system::FileMutation;
use codex_file_system::FileMutationBatch;
use codex_file_system::FileMutationBatchOutcome;
use codex_file_system::FilePreimage;
use codex_utils_path_uri::PathUri;

use crate::protocol::MAX_FS_MUTATE_BATCH_DECODED_BYTES;
use crate::protocol::MAX_FS_MUTATE_BATCH_OPERATIONS;
use std::collections::HashSet;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::io::Read;
use std::io::Seek;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

mod quarantine;
use quarantine::QuarantinedFile;
use quarantine::directory_identity;
use quarantine::discard_quarantined_file;
use quarantine::private_path;
use quarantine::publish_staged_file;
use quarantine::quarantine_path;
use quarantine::remove_created_directory;
use quarantine::restore_quarantined_file;

#[derive(Clone, Debug, Eq, PartialEq)]
struct FilePermissions {
    readonly: bool,
    #[cfg(unix)]
    mode: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Snapshot {
    Missing,
    File {
        contents: Vec<u8>,
        permissions: FilePermissions,
        identity: Option<FileIdentity>,
    },
}

struct PlannedMutation {
    mutation: FileMutation,
    preimage: Snapshot,
}

struct MutationAttempt {
    result: io::Result<()>,
    postimage: io::Result<Snapshot>,
    quarantine: Option<QuarantinedFile>,
}

struct JournalEntry {
    path: PathBuf,
    preimage: Snapshot,
    postimage: Snapshot,
    quarantine: Option<QuarantinedFile>,
}

struct CreatedDirectory {
    path: PathBuf,
    identity: Option<FileIdentity>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    first: u64,
    second: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Checkpoint {
    AfterPreflight,
    BeforeWriteMutation(usize),
    AfterWriteTruncate(usize),
    BeforeWritePublish(usize),
    AfterWritePublish(usize),
    BeforeRemove(usize),
    BeforeRemoveRename(usize),
    AfterMutation(usize),
    BeforeRollback(usize),
    BeforeRollbackRemoveRename(usize),
    BeforeRollbackRestoreRename(usize),
    BeforeCreatedDirectoryRename(usize),
}

#[derive(Clone, Copy)]
enum WriteCheckpoint {
    BeforeMutation,
    AfterTruncate,
    BeforePublish,
    AfterPublish,
}

#[derive(Clone, Copy)]
enum RemoveCheckpoint {
    BeforeRemove,
    BeforeRename,
}

pub(crate) fn mutate_batch(batch: FileMutationBatch) -> FileMutationBatchOutcome {
    mutate_batch_with_hook(batch, |_| Ok(()))
}

fn mutate_batch_with_hook(
    batch: FileMutationBatch,
    mut hook: impl FnMut(Checkpoint) -> io::Result<()>,
) -> FileMutationBatchOutcome {
    let planned = match preflight(batch) {
        Ok(planned) => planned,
        Err(error) => {
            return FileMutationBatchOutcome::Rejected {
                error: error.to_string(),
            };
        }
    };
    if let Err(error) = hook(Checkpoint::AfterPreflight) {
        return FileMutationBatchOutcome::Rejected {
            error: error.to_string(),
        };
    }

    let mut journal = Vec::with_capacity(planned.len());
    let mut created_directories = Vec::new();
    for (index, planned) in planned.into_iter().enumerate() {
        let path = match &planned.mutation {
            FileMutation::Write { path, .. } | FileMutation::Remove { path, .. } => path.clone(),
        };
        let result = apply_mutation(
            planned,
            index,
            &mut journal,
            &mut created_directories,
            &mut hook,
        )
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "filesystem mutation {index} for `{}` failed: {error}",
                    path.inferred_native_path_string()
                ),
            )
        })
        .and_then(|()| hook(Checkpoint::AfterMutation(index)));
        if let Err(error) = result {
            return rollback(error, journal, created_directories, &mut hook);
        }
    }
    finalize_commit(journal)
}

fn preflight(batch: FileMutationBatch) -> io::Result<Vec<PlannedMutation>> {
    preflight_with_limits(
        batch,
        MAX_FS_MUTATE_BATCH_OPERATIONS,
        MAX_FS_MUTATE_BATCH_DECODED_BYTES,
    )
}

fn preflight_with_limits(
    batch: FileMutationBatch,
    max_mutations: usize,
    max_batch_bytes: usize,
) -> io::Result<Vec<PlannedMutation>> {
    if batch.mutations.len() > max_mutations {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("filesystem mutation batch exceeds {max_mutations} operations"),
        ));
    }
    let mut decoded_bytes = 0usize;
    let mut paths = HashSet::with_capacity(batch.mutations.len());
    let mut planned = Vec::with_capacity(batch.mutations.len());
    for mutation in batch.mutations {
        let (path_uri, expected_bytes, contents_bytes) = match &mutation {
            FileMutation::Write {
                path,
                expected,
                contents,
            } => (
                path,
                match expected {
                    FilePreimage::Missing => 0,
                    FilePreimage::Exact(contents) => contents.len(),
                },
                contents.len(),
            ),
            FileMutation::Remove { path, expected } => (path, expected.len(), 0),
        };
        decoded_bytes = decoded_bytes
            .checked_add(expected_bytes)
            .and_then(|bytes| bytes.checked_add(contents_bytes))
            .ok_or_else(|| batch_too_large_error(max_batch_bytes))?;
        if decoded_bytes > max_batch_bytes {
            return Err(batch_too_large_error(max_batch_bytes));
        }
        let path = path_uri.to_abs_path()?.into_path_buf();
        if !paths.insert(path.clone()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "filesystem mutation batch targets `{}` more than once",
                    path.display()
                ),
            ));
        }
        validate_parent(path.as_path()).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("failed to validate `{}`: {error}", path.display()),
            )
        })?;
        let remaining_bytes = max_batch_bytes
            .checked_sub(decoded_bytes)
            .ok_or_else(|| batch_too_large_error(max_batch_bytes))?;
        let preimage =
            read_snapshot_with_limit(path.as_path(), Some(remaining_bytes)).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("failed to read preimage `{}`: {error}", path.display()),
                )
            })?;
        if let Snapshot::File { contents, .. } = &preimage {
            decoded_bytes = decoded_bytes
                .checked_add(contents.len())
                .ok_or_else(|| batch_too_large_error(max_batch_bytes))?;
        }
        if !matches_expected(&preimage, &mutation) {
            return Err(io::Error::other(format!(
                "preimage changed for `{}`",
                path.display()
            )));
        }
        planned.push(PlannedMutation { mutation, preimage });
    }
    Ok(planned)
}

fn batch_too_large_error(max_batch_bytes: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("filesystem mutation batch exceeds {max_batch_bytes} decoded bytes"),
    )
}

fn validate_parent(path: &Path) -> io::Result<()> {
    let mut parent = path.parent();
    while let Some(path) = parent {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                parent = path.parent();
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("parent `{}` is not a directory", path.display()),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => parent = path.parent(),
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("failed to inspect parent `{}`: {error}", path.display()),
                ));
            }
        }
    }
    Ok(())
}

fn apply_mutation(
    planned: PlannedMutation,
    index: usize,
    journal: &mut Vec<JournalEntry>,
    created_directories: &mut Vec<CreatedDirectory>,
    hook: &mut impl FnMut(Checkpoint) -> io::Result<()>,
) -> io::Result<()> {
    let path_uri = match &planned.mutation {
        FileMutation::Write { path, .. } | FileMutation::Remove { path, .. } => path,
    };
    let path = path_uri.to_abs_path()?.into_path_buf();
    let attempt = match &planned.mutation {
        FileMutation::Write { contents, .. } => {
            ensure_parent_directories(path.as_path(), created_directories)?;
            let permissions = match &planned.preimage {
                Snapshot::Missing => None,
                Snapshot::File { permissions, .. } => Some(permissions),
            };
            write_file(
                path.as_path(),
                &planned.preimage,
                contents,
                permissions,
                |checkpoint| {
                    hook(match checkpoint {
                        WriteCheckpoint::BeforeMutation => Checkpoint::BeforeWriteMutation(index),
                        WriteCheckpoint::AfterTruncate => Checkpoint::AfterWriteTruncate(index),
                        WriteCheckpoint::BeforePublish => Checkpoint::BeforeWritePublish(index),
                        WriteCheckpoint::AfterPublish => Checkpoint::AfterWritePublish(index),
                    })
                },
            )?
        }
        FileMutation::Remove { .. } => {
            remove_file(path.as_path(), &planned.preimage, |checkpoint| {
                hook(match checkpoint {
                    RemoveCheckpoint::BeforeRemove => Checkpoint::BeforeRemove(index),
                    RemoveCheckpoint::BeforeRename => Checkpoint::BeforeRemoveRename(index),
                })
            })?
        }
    };
    let MutationAttempt {
        result,
        postimage,
        quarantine,
    } = attempt;
    match (result, postimage) {
        (Ok(()), Ok(postimage)) => {
            journal.push(JournalEntry {
                path,
                preimage: planned.preimage,
                postimage,
                quarantine,
            });
            Ok(())
        }
        (Err(error), Ok(postimage)) => {
            if postimage != planned.preimage || quarantine.is_some() {
                journal.push(JournalEntry {
                    path,
                    preimage: planned.preimage,
                    postimage,
                    quarantine,
                });
            }
            Err(error)
        }
        (result, Err(inspect_error)) => {
            journal.push(JournalEntry {
                path,
                preimage: planned.preimage,
                postimage: Snapshot::Missing,
                quarantine,
            });
            Err(result.err().unwrap_or(inspect_error))
        }
    }
}

fn write_file(
    path: &Path,
    expected: &Snapshot,
    contents: &[u8],
    permissions: Option<&FilePermissions>,
    mut checkpoint: impl FnMut(WriteCheckpoint) -> io::Result<()>,
) -> io::Result<MutationAttempt> {
    if matches!(expected, Snapshot::Missing) {
        return write_missing_file(path, contents, checkpoint);
    }
    validate_parent(path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    configure_no_follow(&mut options);
    let mut file = options.open(path)?;
    let opened = read_snapshot_from_file(&mut file, path)?;
    if matches!(expected, Snapshot::File { .. }) && &opened != expected {
        return Err(preimage_changed_error(path));
    }
    let mut result = (|| {
        checkpoint(WriteCheckpoint::BeforeMutation)?;
        validate_parent(path)?;
        if read_snapshot(path)? != opened {
            return Err(preimage_changed_error(path));
        }
        file.set_len(0)?;
        file.rewind()?;
        checkpoint(WriteCheckpoint::AfterTruncate)?;
        file.write_all(contents)?;
        if let Some(permissions) = permissions {
            set_file_permissions(&file, permissions)?;
        }
        Ok(())
    })();
    let postimage = read_snapshot_from_file(&mut file, path);
    if result.is_ok()
        && let Ok(postimage) = &postimage
    {
        match validate_parent(path).and_then(|()| read_snapshot(path)) {
            Ok(current) if current == *postimage => {}
            Ok(_) | Err(_) => {
                result = Err(io::Error::other(format!(
                    "path `{}` changed during write",
                    path.display()
                )));
            }
        }
    }
    Ok(MutationAttempt {
        result,
        postimage,
        quarantine: None,
    })
}

fn write_missing_file(
    path: &Path,
    contents: &[u8],
    mut checkpoint: impl FnMut(WriteCheckpoint) -> io::Result<()>,
) -> io::Result<MutationAttempt> {
    validate_parent(path)?;
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory")
    })?;
    let parent_identity = directory_identity(parent)?.ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "parent directory does not exist")
    })?;
    let staged_path = private_path(path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    configure_no_follow(&mut options);
    let mut file = options.open(staged_path.as_path())?;
    let mut published = false;
    let mut result = (|| {
        checkpoint(WriteCheckpoint::BeforeMutation)?;
        file.set_len(0)?;
        file.rewind()?;
        checkpoint(WriteCheckpoint::AfterTruncate)?;
        file.write_all(contents)?;
        let staged = read_snapshot_from_file(&mut file, path)?;
        validate_parent(path)?;
        if directory_identity(parent)? != Some(parent_identity)
            || read_snapshot(path)? != Snapshot::Missing
            || read_snapshot(staged_path.as_path())? != staged
        {
            return Err(preimage_changed_error(path));
        }
        checkpoint(WriteCheckpoint::BeforePublish)?;
        publish_staged_file(staged_path.as_path(), path)?;
        published = true;
        checkpoint(WriteCheckpoint::AfterPublish)?;
        validate_parent(path)?;
        if directory_identity(parent)? != Some(parent_identity)
            || read_snapshot(path)? != staged
            || read_snapshot(staged_path.as_path())? != Snapshot::Missing
        {
            return Err(io::Error::other(format!(
                "path `{}` changed during staged write",
                path.display()
            )));
        }
        Ok(())
    })();
    let staged = read_snapshot_from_file(&mut file, path);
    let postimage = if published {
        staged.as_ref().map(Clone::clone).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("could not inspect staged file: {error}"),
            )
        })
    } else {
        validate_parent(path).and_then(|()| read_snapshot(path))
    };
    let quarantine = if result.is_err() && !published {
        Some(QuarantinedFile::staged(staged_path, staged.ok()))
    } else {
        None
    };
    if result.is_ok() && postimage.is_err() {
        result = Err(io::Error::other(format!(
            "could not inspect path `{}` after staged write",
            path.display()
        )));
    }
    Ok(MutationAttempt {
        result,
        postimage,
        quarantine,
    })
}

fn remove_file(
    path: &Path,
    expected: &Snapshot,
    mut checkpoint: impl FnMut(RemoveCheckpoint) -> io::Result<()>,
) -> io::Result<MutationAttempt> {
    validate_parent(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    configure_no_follow(&mut options);
    let mut file = options.open(path)?;
    let opened = read_snapshot_from_file(&mut file, path)?;
    if &opened != expected {
        return Err(preimage_changed_error(path));
    }
    checkpoint(RemoveCheckpoint::BeforeRemove)?;
    validate_parent(path)?;
    if read_snapshot(path)? != opened {
        return Err(preimage_changed_error(path));
    }
    checkpoint(RemoveCheckpoint::BeforeRename)?;

    let quarantine_path = quarantine_path(path)?;
    let moved = read_snapshot(quarantine_path.as_path());
    let postimage = validate_parent(path).and_then(|()| read_snapshot(path));
    if let (Ok(moved), Ok(Snapshot::Missing)) = (&moved, &postimage)
        && moved != &opened
        && restore_quarantined_file(path, quarantine_path.as_path(), moved, || Ok(())).is_ok()
    {
        return Err(preimage_changed_error(path));
    }
    let result = match (&moved, &postimage) {
        (Ok(moved), Ok(Snapshot::Missing)) if moved == &opened => Ok(()),
        _ => Err(io::Error::other(format!(
            "path `{}` changed during removal",
            path.display()
        ))),
    };
    Ok(MutationAttempt {
        result,
        postimage,
        quarantine: Some(QuarantinedFile::removed(quarantine_path, moved.ok())),
    })
}

fn preimage_changed_error(path: &Path) -> io::Error {
    io::Error::other(format!(
        "preimage contents, permissions, or identity changed for `{}` during commit",
        path.display()
    ))
}

fn ensure_parent_directories(path: &Path, created: &mut Vec<CreatedDirectory>) -> io::Result<()> {
    let mut missing = Vec::new();
    let mut parent = path.parent();
    while let Some(path) = parent {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => break,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("parent `{}` is not a directory", path.display()),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(path.to_path_buf());
                parent = path.parent();
            }
            Err(error) => return Err(error),
        }
    }
    for path in missing.into_iter().rev() {
        match std::fs::create_dir(path.as_path()) {
            Ok(()) => {
                let metadata = std::fs::symlink_metadata(path.as_path())?;
                created.push(CreatedDirectory {
                    identity: path_identity(path.as_path(), &metadata)?,
                    path,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let metadata = std::fs::symlink_metadata(path.as_path())?;
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn rollback(
    commit_error: io::Error,
    journal: Vec<JournalEntry>,
    created_directories: Vec<CreatedDirectory>,
    hook: &mut impl FnMut(Checkpoint) -> io::Result<()>,
) -> FileMutationBatchOutcome {
    let mut rollback_errors = Vec::new();
    let mut possibly_mutated_paths = Vec::new();
    for (index, entry) in journal.into_iter().rev().enumerate() {
        let result = hook(Checkpoint::BeforeRollback(index))
            .and_then(|()| restore_entry(&entry, index, hook));
        if let Err(error) = result {
            if let Ok(path) = PathUri::from_host_native_path(&entry.path) {
                possibly_mutated_paths.push(path);
            }
            rollback_errors.push(format!("{}: {error}", entry.path.display()));
        }
    }
    for (index, directory) in created_directories.into_iter().rev().enumerate() {
        if let Err(error) = remove_created_directory(&directory, || {
            hook(Checkpoint::BeforeCreatedDirectoryRename(index))
        }) {
            if let Ok(path) = PathUri::from_host_native_path(&directory.path) {
                possibly_mutated_paths.push(path);
            }
            rollback_errors.push(format!("{}: {error}", directory.path.display()));
        }
    }
    if rollback_errors.is_empty() {
        FileMutationBatchOutcome::RolledBack {
            error: commit_error.to_string(),
        }
    } else {
        FileMutationBatchOutcome::Indeterminate {
            error: format!(
                "{commit_error}; rollback failed: {}",
                rollback_errors.join("; ")
            ),
            possibly_mutated_paths,
        }
    }
}

fn restore_entry(
    entry: &JournalEntry,
    index: usize,
    hook: &mut impl FnMut(Checkpoint) -> io::Result<()>,
) -> io::Result<()> {
    if let Some(quarantine) = &entry.quarantine {
        let snapshot = quarantine.snapshot().ok_or_else(|| {
            io::Error::other("could not inspect quarantined file before rollback")
        })?;
        if !quarantine.restore_on_rollback() {
            return discard_quarantined_file(quarantine.path(), snapshot);
        }
        if snapshot != &entry.preimage {
            return Err(io::Error::other(
                "a concurrent replacement was retained in quarantine",
            ));
        }
        return restore_quarantined_file(entry.path.as_path(), quarantine.path(), snapshot, || {
            hook(Checkpoint::BeforeRollbackRestoreRename(index))
        });
    }
    match &entry.preimage {
        Snapshot::Missing => {
            let attempt =
                remove_file(
                    entry.path.as_path(),
                    &entry.postimage,
                    |checkpoint| match checkpoint {
                        RemoveCheckpoint::BeforeRemove => Ok(()),
                        RemoveCheckpoint::BeforeRename => {
                            hook(Checkpoint::BeforeRollbackRemoveRename(index))
                        }
                    },
                )?;
            attempt.result?;
            if attempt.postimage? != Snapshot::Missing {
                return Err(io::Error::other("path changed during rollback removal"));
            }
            let quarantine = attempt.quarantine.ok_or_else(|| {
                io::Error::other("rollback removal did not retain a quarantined file")
            })?;
            let snapshot = quarantine
                .snapshot()
                .ok_or_else(|| io::Error::other("could not inspect rollback quarantine"))?;
            discard_quarantined_file(quarantine.path(), snapshot)?;
            Ok(())
        }
        Snapshot::File {
            contents,
            permissions,
            ..
        } => {
            let attempt = write_file(
                entry.path.as_path(),
                &entry.postimage,
                contents,
                Some(permissions),
                |_| Ok(()),
            )?;
            attempt.result?;
            let restored = attempt.postimage?;
            if read_snapshot(entry.path.as_path())? != restored {
                return Err(io::Error::other("path changed during rollback write"));
            }
            Ok(())
        }
    }
}

fn finalize_commit(journal: Vec<JournalEntry>) -> FileMutationBatchOutcome {
    let mut errors = Vec::new();
    let possibly_mutated_paths = journal
        .iter()
        .filter_map(|entry| PathUri::from_host_native_path(&entry.path).ok())
        .collect();
    for entry in journal {
        let Some(quarantine) = entry.quarantine else {
            continue;
        };
        let result = quarantine
            .snapshot()
            .ok_or_else(|| io::Error::other("could not inspect commit quarantine"))
            .and_then(|snapshot| discard_quarantined_file(quarantine.path(), snapshot));
        if let Err(error) = result {
            errors.push(format!("{}: {error}", entry.path.display()));
        }
    }
    if errors.is_empty() {
        FileMutationBatchOutcome::Committed
    } else {
        FileMutationBatchOutcome::Indeterminate {
            error: format!("commit quarantine cleanup failed: {}", errors.join("; ")),
            possibly_mutated_paths,
        }
    }
}

fn matches_expected(snapshot: &Snapshot, mutation: &FileMutation) -> bool {
    match (snapshot, mutation) {
        (
            Snapshot::Missing,
            FileMutation::Write {
                expected: FilePreimage::Missing,
                ..
            },
        ) => true,
        (
            Snapshot::File { contents, .. },
            FileMutation::Write {
                expected: FilePreimage::Exact(expected),
                ..
            }
            | FileMutation::Remove { expected, .. },
        ) => contents == expected,
        _ => false,
    }
}

fn read_snapshot(path: &Path) -> io::Result<Snapshot> {
    read_snapshot_with_limit(path, None)
}

fn read_snapshot_with_limit(path: &Path, max_bytes: Option<usize>) -> io::Result<Snapshot> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_no_follow(&mut options);
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Snapshot::Missing),
        Err(error) => return Err(error),
    };
    read_snapshot_from_file_with_limit(&mut file, path, max_bytes)
}

fn read_snapshot_from_file(file: &mut File, path: &Path) -> io::Result<Snapshot> {
    read_snapshot_from_file_with_limit(file, path, None)
}

fn read_snapshot_from_file_with_limit(
    file: &mut File,
    path: &Path,
    max_bytes: Option<usize>,
) -> io::Result<Snapshot> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path `{}` is not a regular file", path.display()),
        ));
    }
    if let Some(max_bytes) = max_bytes
        && metadata.len() > max_bytes as u64
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "filesystem mutation batch preimage exceeds remaining decoded byte limit",
        ));
    }
    file.rewind()?;
    let capacity = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
    let mut contents = Vec::with_capacity(max_bytes.map_or(capacity, |limit| capacity.min(limit)));
    if let Some(max_bytes) = max_bytes {
        let read_limit = u64::try_from(max_bytes.saturating_add(1)).unwrap_or(u64::MAX);
        file.take(read_limit).read_to_end(&mut contents)?;
        if contents.len() > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "filesystem mutation batch preimage exceeds remaining decoded byte limit",
            ));
        }
    } else {
        file.read_to_end(&mut contents)?;
    }
    Ok(Snapshot::File {
        contents,
        permissions: permissions(&metadata),
        identity: open_file_identity(file, &metadata)?,
    })
}

fn permissions(metadata: &std::fs::Metadata) -> FilePermissions {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    FilePermissions {
        readonly: metadata.permissions().readonly(),
        #[cfg(unix)]
        mode: metadata.permissions().mode(),
    }
}

fn set_file_permissions(file: &std::fs::File, permissions: &FilePermissions) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(permissions.mode))
    }
    #[cfg(not(unix))]
    {
        let mut current = file.metadata()?.permissions();
        current.set_readonly(permissions.readonly);
        file.set_permissions(current)
    }
}

#[cfg(unix)]
fn configure_no_follow(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.custom_flags(libc::O_NOFOLLOW);
}

#[cfg(windows)]
fn configure_no_follow(options: &mut OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
}

#[cfg(not(any(unix, windows)))]
fn configure_no_follow(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn open_file_identity(
    _file: &File,
    metadata: &std::fs::Metadata,
) -> io::Result<Option<FileIdentity>> {
    use std::os::unix::fs::MetadataExt;
    Ok(Some(FileIdentity {
        first: metadata.dev(),
        second: metadata.ino(),
    }))
}

#[cfg(windows)]
fn open_file_identity(
    file: &File,
    _metadata: &std::fs::Metadata,
) -> io::Result<Option<FileIdentity>> {
    use std::mem::MaybeUninit;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION;
    use windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandle;

    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // SAFETY: `information` points to writable storage for the required structure and remains
    // alive for the duration of the call. A successful call initializes the whole structure.
    let result = unsafe {
        GetFileInformationByHandle(file.as_raw_handle() as isize, information.as_mut_ptr())
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: GetFileInformationByHandle returned success, so `information` is initialized.
    let information = unsafe { information.assume_init() };
    Ok(Some(FileIdentity {
        first: u64::from(information.dwVolumeSerialNumber),
        second: u64::from(information.nFileIndexHigh) << 32 | u64::from(information.nFileIndexLow),
    }))
}

#[cfg(not(any(unix, windows)))]
fn open_file_identity(
    _file: &File,
    _metadata: &std::fs::Metadata,
) -> io::Result<Option<FileIdentity>> {
    Ok(None)
}

#[cfg(windows)]
fn path_identity(path: &Path, metadata: &std::fs::Metadata) -> io::Result<Option<FileIdentity>> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
    use windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES;

    let mut options = OpenOptions::new();
    options
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "failed to open directory `{}` for identity: {error}",
                path.display()
            ),
        )
    })?;
    open_file_identity(&file, metadata)
}

#[cfg(not(windows))]
fn path_identity(_path: &Path, metadata: &std::fs::Metadata) -> io::Result<Option<FileIdentity>> {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

    #[cfg(unix)]
    return Ok(Some(FileIdentity {
        first: metadata.dev(),
        second: metadata.ino(),
    }));
    #[cfg(not(unix))]
    Ok(None)
}

#[cfg(test)]
#[path = "file_mutation_batch_tests.rs"]
mod tests;
