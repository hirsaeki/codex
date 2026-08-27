use std::collections::HashSet;
use std::io;
use std::path::PathBuf;

use codex_utils_path_uri::PathUri;

use crate::CreateDirectoryOptions;
use crate::ExecutorFileSystem;
use crate::FileMetadata;
use crate::FileMutation;
use crate::FileMutationBatch;
use crate::FileMutationBatchOutcome;
use crate::FilePreimage;
use crate::GetMetadataOptions;
use crate::ReadFileOptions;
use crate::RemoveOptions;
use crate::WriteFileOptions;
use crate::local_file_system::DirectFileSystem;
use crate::protocol::MAX_FS_MUTATE_BATCH_DECODED_BYTES;
use crate::protocol::MAX_FS_MUTATE_BATCH_OPERATIONS;

#[derive(Clone, Debug, Eq, PartialEq)]
enum Snapshot {
    Missing,
    Exact(Vec<u8>),
}

#[derive(Clone)]
struct PlannedMutation {
    mutation: FileMutation,
    preimage: Snapshot,
}

#[derive(Clone)]
struct JournalEntry {
    path: PathUri,
    preimage: Snapshot,
    postimage: Snapshot,
}

#[derive(Clone)]
struct CreatedDirectory {
    path: PathUri,
    metadata: FileMetadata,
}

pub(crate) async fn mutate_batch(batch: FileMutationBatch) -> FileMutationBatchOutcome {
    mutate_batch_on(&DirectFileSystem, batch).await
}

async fn mutate_batch_on(
    file_system: &dyn ExecutorFileSystem,
    batch: FileMutationBatch,
) -> FileMutationBatchOutcome {
    let follow_symlinks = batch.follow_symlinks;
    let planned = match preflight(file_system, batch).await {
        Ok(planned) => planned,
        Err(error) => {
            return FileMutationBatchOutcome::Rejected {
                error: error.to_string(),
            };
        }
    };

    let mut journal = Vec::with_capacity(planned.len());
    let mut created_directories = Vec::new();
    for (index, planned) in planned.into_iter().enumerate() {
        let path = mutation_path(&planned.mutation).clone();
        let result = apply_mutation(
            file_system,
            planned,
            follow_symlinks,
            &mut created_directories,
        )
        .await;
        match result {
            Ok(postimage) => journal.push(JournalEntry {
                path,
                preimage: match mutation_expected_snapshot(&journal, &path) {
                    Some(snapshot) => snapshot,
                    None => snapshot_from_expected(&planned_expected(&postimage, &path)),
                },
                postimage,
            }),
            Err(failure) => {
                let error = io::Error::new(
                    failure.error.kind(),
                    format!(
                        "filesystem mutation {index} for `{}` failed: {}",
                        path.inferred_native_path_string(),
                        failure.error
                    ),
                );
                return rollback(
                    file_system,
                    error,
                    failure.entry,
                    journal,
                    created_directories,
                    follow_symlinks,
                )
                .await;
            }
        }
    }

    FileMutationBatchOutcome::Committed
}

// Kept separate so tests can exercise preflight without performing mutations.
async fn preflight(
    file_system: &dyn ExecutorFileSystem,
    batch: FileMutationBatch,
) -> io::Result<Vec<PlannedMutation>> {
    if batch.mutations.len() > MAX_FS_MUTATE_BATCH_OPERATIONS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "filesystem mutation batch exceeds {MAX_FS_MUTATE_BATCH_OPERATIONS} operations"
            ),
        ));
    }

    let mut decoded_bytes = 0usize;
    let mut paths = HashSet::<PathBuf>::with_capacity(batch.mutations.len());
    let mut planned = Vec::with_capacity(batch.mutations.len());
    for mutation in batch.mutations {
        decoded_bytes = decoded_bytes
            .checked_add(mutation_payload_bytes(&mutation))
            .ok_or_else(batch_too_large_error)?;
        if decoded_bytes > MAX_FS_MUTATE_BATCH_DECODED_BYTES {
            return Err(batch_too_large_error());
        }
        let path = mutation_path(&mutation);
        let native = path.to_abs_path()?.into_path_buf();
        if !paths.insert(native.clone()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "filesystem mutation batch targets `{}` more than once",
                    native.display()
                ),
            ));
        }
        let preimage = snapshot(file_system, path, batch.follow_symlinks).await?;
        decoded_bytes = decoded_bytes
            .checked_add(snapshot_bytes(&preimage))
            .ok_or_else(batch_too_large_error)?;
        if decoded_bytes > MAX_FS_MUTATE_BATCH_DECODED_BYTES {
            return Err(batch_too_large_error());
        }
        if !matches_expected(&preimage, &mutation) {
            return Err(io::Error::other(format!(
                "preimage changed for `{}`",
                path.inferred_native_path_string()
            )));
        }
        planned.push(PlannedMutation { mutation, preimage });
    }
    Ok(planned)
}

fn batch_too_large_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "filesystem mutation batch exceeds {MAX_FS_MUTATE_BATCH_DECODED_BYTES} decoded bytes"
        ),
    )
}

fn mutation_payload_bytes(mutation: &FileMutation) -> usize {
    match mutation {
        FileMutation::Write {
            expected, contents, ..
        } => expected_bytes(expected).saturating_add(contents.len()),
        FileMutation::Remove { expected, .. } => expected.len(),
    }
}

fn expected_bytes(expected: &FilePreimage) -> usize {
    match expected {
        FilePreimage::Missing => 0,
        FilePreimage::Exact(contents) => contents.len(),
    }
}

fn snapshot_bytes(snapshot: &Snapshot) -> usize {
    match snapshot {
        Snapshot::Missing => 0,
        Snapshot::Exact(contents) => contents.len(),
    }
}

fn mutation_path(mutation: &FileMutation) -> &PathUri {
    match mutation {
        FileMutation::Write { path, .. } | FileMutation::Remove { path, .. } => path,
    }
}

fn expected_snapshot(mutation: &FileMutation) -> Snapshot {
    match mutation {
        FileMutation::Write { expected, .. } => match expected {
            FilePreimage::Missing => Snapshot::Missing,
            FilePreimage::Exact(contents) => Snapshot::Exact(contents.clone()),
        },
        FileMutation::Remove { expected, .. } => Snapshot::Exact(expected.clone()),
    }
}

fn intended_postimage(mutation: &FileMutation) -> Snapshot {
    match mutation {
        FileMutation::Write { contents, .. } => Snapshot::Exact(contents.clone()),
        FileMutation::Remove { .. } => Snapshot::Missing,
    }
}

fn matches_expected(snapshot: &Snapshot, mutation: &FileMutation) -> bool {
    snapshot == &expected_snapshot(mutation)
}

async fn snapshot(
    file_system: &dyn ExecutorFileSystem,
    path: &PathUri,
    follow_symlinks: bool,
) -> io::Result<Snapshot> {
    let metadata = match file_system
        .get_metadata(
            path,
            GetMetadataOptions { follow_symlinks },
            /*sandbox*/ None,
        )
        .await
    {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Snapshot::Missing),
        Err(error) => return Err(error),
    };
    if !metadata.is_file {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("`{}` is not a regular file", path.inferred_native_path_string()),
        ));
    }
    let contents = file_system
        .read_file(
            path,
            ReadFileOptions { follow_symlinks },
            /*sandbox*/ None,
        )
        .await?;
    Ok(Snapshot::Exact(contents))
}

struct MutationFailure {
    error: io::Error,
    entry: Option<JournalEntry>,
}

async fn apply_mutation(
    file_system: &dyn ExecutorFileSystem,
    planned: PlannedMutation,
    follow_symlinks: bool,
    created_directories: &mut Vec<CreatedDirectory>,
) -> Result<Snapshot, MutationFailure> {
    let path = mutation_path(&planned.mutation).clone();
    if matches!(planned.mutation, FileMutation::Write { .. }) {
        if let Err(error) = ensure_parent_directories(
            file_system,
            &path,
            follow_symlinks,
            created_directories,
        )
        .await
        {
            return Err(MutationFailure { error, entry: None });
        }
    }

    let current = match snapshot(file_system, &path, follow_symlinks).await {
        Ok(current) => current,
        Err(error) => return Err(MutationFailure { error, entry: None }),
    };
    if current != planned.preimage {
        return Err(MutationFailure {
            error: io::Error::other(format!(
                "preimage changed for `{}` before mutation",
                path.inferred_native_path_string()
            )),
            entry: None,
        });
    }

    let intended = intended_postimage(&planned.mutation);
    let operation = match &planned.mutation {
        FileMutation::Write { contents, .. } => {
            file_system
                .write_file(
                    &path,
                    contents.clone(),
                    WriteFileOptions { follow_symlinks },
                    /*sandbox*/ None,
                )
                .await
        }
        FileMutation::Remove { .. } => {
            file_system
                .remove(
                    &path,
                    RemoveOptions {
                        recursive: false,
                        force: false,
                        follow_symlinks,
                    },
                    /*sandbox*/ None,
                )
                .await
        }
    };

    let observed = snapshot(file_system, &path, follow_symlinks).await;
    match (operation, observed) {
        (Ok(()), Ok(observed)) if observed == intended => Ok(observed),
        (Ok(()), Ok(observed)) => Err(MutationFailure {
            error: io::Error::other(format!(
                "postimage changed for `{}`",
                path.inferred_native_path_string()
            )),
            entry: Some(JournalEntry {
                path,
                preimage: planned.preimage,
                postimage: observed,
            }),
        }),
        (Ok(()), Err(error)) => Err(MutationFailure {
            error,
            entry: Some(JournalEntry {
                path,
                preimage: planned.preimage,
                postimage: intended,
            }),
        }),
        (Err(error), Ok(observed)) => Err(MutationFailure {
            error,
            entry: (observed != planned.preimage).then_some(JournalEntry {
                path,
                preimage: planned.preimage,
                postimage: observed,
            }),
        }),
        (Err(error), Err(_)) => Err(MutationFailure {
            error,
            entry: Some(JournalEntry {
                path,
                preimage: planned.preimage,
                postimage: intended,
            }),
        }),
    }
}

async fn ensure_parent_directories(
    file_system: &dyn ExecutorFileSystem,
    path: &PathUri,
    follow_symlinks: bool,
    created: &mut Vec<CreatedDirectory>,
) -> io::Result<()> {
    let native = path.to_abs_path()?;
    let mut missing = Vec::<PathBuf>::new();
    let mut parent = native.as_path().parent();
    while let Some(candidate) = parent {
        let candidate_uri = PathUri::from_host_native_path(candidate)?;
        match file_system
            .get_metadata(
                &candidate_uri,
                GetMetadataOptions { follow_symlinks },
                /*sandbox*/ None,
            )
            .await
        {
            Ok(metadata) if metadata.is_directory => break,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("parent `{}` is not a directory", candidate.display()),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(candidate.to_path_buf());
                parent = candidate.parent();
            }
            Err(error) => return Err(error),
        }
    }

    for path in missing.into_iter().rev() {
        let path_uri = PathUri::from_host_native_path(&path)?;
        match file_system
            .create_directory(
                &path_uri,
                CreateDirectoryOptions {
                    recursive: false,
                    follow_symlinks,
                },
                /*sandbox*/ None,
            )
            .await
        {
            Ok(()) => {
                let metadata = file_system
                    .get_metadata(
                        &path_uri,
                        GetMetadataOptions { follow_symlinks },
                        /*sandbox*/ None,
                    )
                    .await?;
                if !metadata.is_directory || metadata.is_symlink {
                    return Err(io::Error::other(format!(
                        "created parent `{}` is not a stable directory",
                        path.display()
                    )));
                }
                created.push(CreatedDirectory {
                    path: path_uri,
                    metadata,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let metadata = file_system
                    .get_metadata(
                        &path_uri,
                        GetMetadataOptions { follow_symlinks },
                        /*sandbox*/ None,
                    )
                    .await?;
                if !metadata.is_directory {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

async fn rollback(
    file_system: &dyn ExecutorFileSystem,
    cause: io::Error,
    failed_entry: Option<JournalEntry>,
    mut journal: Vec<JournalEntry>,
    created_directories: Vec<CreatedDirectory>,
    follow_symlinks: bool,
) -> FileMutationBatchOutcome {
    if let Some(entry) = failed_entry {
        journal.push(entry);
    }
    let mut possibly_mutated_paths = Vec::new();
    let mut rollback_errors = Vec::new();

    for entry in journal.into_iter().rev() {
        if let Err(error) = rollback_entry(file_system, &entry, follow_symlinks).await {
            possibly_mutated_paths.push(entry.path.clone());
            rollback_errors.push(format!(
                "failed to roll back `{}`: {error}",
                entry.path.inferred_native_path_string()
            ));
        }
    }

    for directory in created_directories.into_iter().rev() {
        if let Err(error) = rollback_created_directory(file_system, &directory, follow_symlinks).await
        {
            rollback_errors.push(format!(
                "failed to roll back created directory `{}`: {error}",
                directory.path.inferred_native_path_string()
            ));
        }
    }

    if rollback_errors.is_empty() {
        FileMutationBatchOutcome::RolledBack {
            error: cause.to_string(),
        }
    } else {
        FileMutationBatchOutcome::Indeterminate {
            error: format!("{}; {}", cause, rollback_errors.join("; ")),
            possibly_mutated_paths,
        }
    }
}

async fn rollback_entry(
    file_system: &dyn ExecutorFileSystem,
    entry: &JournalEntry,
    follow_symlinks: bool,
) -> io::Result<()> {
    let current = snapshot(file_system, &entry.path, follow_symlinks).await?;
    if current == entry.preimage {
        return Ok(());
    }
    if current != entry.postimage {
        return Err(io::Error::other(
            "filesystem changed after the batch mutation; refusing to overwrite it during rollback",
        ));
    }

    match &entry.preimage {
        Snapshot::Missing => {
            file_system
                .remove(
                    &entry.path,
                    RemoveOptions {
                        recursive: false,
                        force: false,
                        follow_symlinks,
                    },
                    /*sandbox*/ None,
                )
                .await?;
        }
        Snapshot::Exact(contents) => {
            file_system
                .write_file(
                    &entry.path,
                    contents.clone(),
                    WriteFileOptions { follow_symlinks },
                    /*sandbox*/ None,
                )
                .await?;
        }
    }

    let restored = snapshot(file_system, &entry.path, follow_symlinks).await?;
    if restored == entry.preimage {
        Ok(())
    } else {
        Err(io::Error::other("rollback verification failed"))
    }
}

async fn rollback_created_directory(
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

// These helpers intentionally remain tiny; they make ownership moves in the main loop explicit.
fn mutation_expected_snapshot(_journal: &[JournalEntry], _path: &PathUri) -> Option<Snapshot> {
    None
}

fn planned_expected(snapshot: &Snapshot, _path: &PathUri) -> FilePreimage {
    match snapshot {
        Snapshot::Missing => FilePreimage::Missing,
        Snapshot::Exact(contents) => FilePreimage::Exact(contents.clone()),
    }
}

fn snapshot_from_expected(expected: &FilePreimage) -> Snapshot {
    match expected {
        FilePreimage::Missing => Snapshot::Missing,
        FilePreimage::Exact(contents) => Snapshot::Exact(contents.clone()),
    }
}
