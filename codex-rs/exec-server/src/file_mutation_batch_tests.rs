use std::io;
use std::path::Path;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::*;
use crate::CopyOptions;
use crate::ExecutorFileSystemFuture;
use crate::FileSystemReadStream;
use crate::FileSystemResult;
use crate::ReadDirectoryEntry;
use crate::WalkOptions;
use crate::WalkOutcome;

fn path_uri(path: &Path) -> PathUri {
    PathUri::from_host_native_path(path).expect("temporary path should be absolute")
}

fn write(path: &Path, expected: FilePreimage, contents: &[u8]) -> FileMutation {
    FileMutation::Write {
        path: path_uri(path),
        expected,
        contents: contents.to_vec(),
    }
}

fn remove(path: &Path, expected: &[u8]) -> FileMutation {
    FileMutation::Remove {
        path: path_uri(path),
        expected: expected.to_vec(),
    }
}

fn batch(mutations: Vec<FileMutation>, follow_symlinks: bool) -> FileMutationBatch {
    FileMutationBatch {
        mutations,
        follow_symlinks,
    }
}

#[tokio::test]
async fn empty_batch_commits() {
    assert_eq!(mutate_batch(batch(Vec::new(), false)).await, FileMutationBatchOutcome::Committed);
}

#[tokio::test]
async fn commits_single_and_mixed_mutations_with_nested_parent_creation() -> io::Result<()> {
    let temp = TempDir::new()?;
    let added = temp.path().join("new/a/b/file.txt");
    let updated = temp.path().join("updated.txt");
    let removed = temp.path().join("removed.txt");
    std::fs::write(&updated, b"before")?;
    std::fs::write(&removed, b"delete")?;

    let outcome = mutate_batch(batch(
        vec![
            write(&added, FilePreimage::Missing, b"added"),
            write(&updated, FilePreimage::Exact(b"before".to_vec()), b"after"),
            remove(&removed, b"delete"),
        ],
        false,
    ))
    .await;

    assert_eq!(outcome, FileMutationBatchOutcome::Committed);
    assert_eq!(std::fs::read(added)?, b"added");
    assert_eq!(std::fs::read(updated)?, b"after");
    assert!(!removed.exists());
    Ok(())
}

#[tokio::test]
async fn stale_preimage_rejects_without_mutation() -> io::Result<()> {
    let temp = TempDir::new()?;
    let first = temp.path().join("first.txt");
    let stale = temp.path().join("stale.txt");
    std::fs::write(&first, b"first")?;
    std::fs::write(&stale, b"current")?;

    let outcome = mutate_batch(batch(
        vec![
            write(&first, FilePreimage::Exact(b"first".to_vec()), b"changed"),
            write(&stale, FilePreimage::Exact(b"old".to_vec()), b"new"),
        ],
        false,
    ))
    .await;

    assert!(matches!(outcome, FileMutationBatchOutcome::Rejected { .. }));
    assert_eq!(std::fs::read(first)?, b"first");
    assert_eq!(std::fs::read(stale)?, b"current");
    Ok(())
}

#[tokio::test]
async fn rejects_duplicate_target_and_operation_limit() -> io::Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("file.txt");
    std::fs::write(&path, b"before")?;
    let duplicate = mutate_batch(batch(
        vec![
            write(&path, FilePreimage::Exact(b"before".to_vec()), b"first"),
            write(&path, FilePreimage::Exact(b"before".to_vec()), b"second"),
        ],
        false,
    ))
    .await;
    assert!(matches!(duplicate, FileMutationBatchOutcome::Rejected { .. }));
    assert_eq!(std::fs::read(&path)?, b"before");

    let mutations = (0..=MAX_FS_MUTATE_BATCH_OPERATIONS)
        .map(|index| write(&temp.path().join(format!("{index}.txt")), FilePreimage::Missing, b"x"))
        .collect();
    let too_many = mutate_batch(batch(mutations, false)).await;
    assert!(matches!(too_many, FileMutationBatchOutcome::Rejected { .. }));
    Ok(())
}

#[tokio::test]
async fn rejects_decoded_byte_limit_without_mutation() -> io::Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("file.txt");
    let result = preflight_with_limits(
        &DirectFileSystem,
        batch(vec![write(&path, FilePreimage::Missing, b"12345")], false),
        usize::MAX,
        4,
    )
    .await;
    assert!(result.is_err());
    assert!(!path.exists());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn no_follow_rejects_leaf_and_ancestor_symlinks() -> io::Result<()> {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new()?;
    let target = temp.path().join("target.txt");
    let leaf = temp.path().join("leaf.txt");
    std::fs::write(&target, b"target")?;
    symlink(&target, &leaf)?;
    let leaf_outcome = mutate_batch(batch(
        vec![write(&leaf, FilePreimage::Exact(b"target".to_vec()), b"escaped")],
        false,
    ))
    .await;
    assert!(matches!(leaf_outcome, FileMutationBatchOutcome::Rejected { .. }));
    assert_eq!(std::fs::read(&target)?, b"target");

    let real = temp.path().join("real");
    let ancestor = temp.path().join("ancestor");
    std::fs::create_dir(&real)?;
    symlink(&real, &ancestor)?;
    let nested = ancestor.join("child/file.txt");
    let ancestor_outcome = mutate_batch(batch(
        vec![write(&nested, FilePreimage::Missing, b"escaped")],
        false,
    ))
    .await;
    assert!(matches!(ancestor_outcome, FileMutationBatchOutcome::Rejected { .. }));
    assert!(!real.join("child").exists());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn follow_symlinks_true_preserves_supported_leaf_semantics() -> io::Result<()> {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new()?;
    let target = temp.path().join("target.txt");
    let leaf = temp.path().join("leaf.txt");
    std::fs::write(&target, b"before")?;
    symlink(&target, &leaf)?;

    let outcome = mutate_batch(batch(
        vec![write(&leaf, FilePreimage::Exact(b"before".to_vec()), b"after")],
        true,
    ))
    .await;
    assert_eq!(outcome, FileMutationBatchOutcome::Committed);
    assert_eq!(std::fs::read(target)?, b"after");
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn no_follow_detects_ancestor_swap_between_preflight_and_mutation() -> io::Result<()> {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new()?;
    let safe = temp.path().join("safe");
    let safe_real = temp.path().join("safe-real");
    let outside = temp.path().join("outside");
    std::fs::create_dir(&safe)?;
    std::fs::create_dir(&outside)?;
    std::fs::write(safe.join("file.txt"), b"before")?;
    std::fs::write(outside.join("file.txt"), b"outside")?;
    let path = safe.join("file.txt");

    let mut planned = preflight(
        &DirectFileSystem,
        batch(
            vec![write(&path, FilePreimage::Exact(b"before".to_vec()), b"after")],
            false,
        ),
    )
    .await?;
    std::fs::rename(&safe, &safe_real)?;
    symlink(&outside, &safe)?;

    let result = apply_mutation(
        &DirectFileSystem,
        planned.pop().expect("planned mutation"),
        false,
        &mut Vec::new(),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(std::fs::read(outside.join("file.txt"))?, b"outside");
    assert_eq!(std::fs::read(safe_real.join("file.txt"))?, b"before");
    Ok(())
}

struct FailingFs {
    inner: DirectFileSystem,
    write_calls: AtomicUsize,
    fail_write_call: Option<usize>,
    fail_remove: bool,
}

impl FailingFs {
    fn new(fail_write_call: Option<usize>, fail_remove: bool) -> Self {
        Self {
            inner: DirectFileSystem,
            write_calls: AtomicUsize::new(0),
            fail_write_call,
            fail_remove,
        }
    }
}

impl ExecutorFileSystem for FailingFs {
    fn canonicalize<'a>(
        &'a self,
        path: &'a PathUri,
        sandbox: Option<&'a crate::FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, PathUri> {
        self.inner.canonicalize(path, sandbox)
    }

    fn read_file<'a>(
        &'a self,
        path: &'a PathUri,
        options: ReadFileOptions,
        sandbox: Option<&'a crate::FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, Vec<u8>> {
        self.inner.read_file(path, options, sandbox)
    }

    fn read_file_stream<'a>(
        &'a self,
        path: &'a PathUri,
        sandbox: Option<&'a crate::FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileSystemReadStream> {
        self.inner.read_file_stream(path, sandbox)
    }

    fn write_file<'a>(
        &'a self,
        path: &'a PathUri,
        contents: Vec<u8>,
        options: WriteFileOptions,
        sandbox: Option<&'a crate::FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(async move {
            let call = self.write_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_write_call == Some(call) {
                return Err(io::Error::other("injected write failure"));
            }
            self.inner.write_file(path, contents, options, sandbox).await
        })
    }

    fn create_directory<'a>(
        &'a self,
        path: &'a PathUri,
        options: CreateDirectoryOptions,
        sandbox: Option<&'a crate::FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        self.inner.create_directory(path, options, sandbox)
    }

    fn get_metadata<'a>(
        &'a self,
        path: &'a PathUri,
        options: GetMetadataOptions,
        sandbox: Option<&'a crate::FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileMetadata> {
        self.inner.get_metadata(path, options, sandbox)
    }

    fn read_directory<'a>(
        &'a self,
        path: &'a PathUri,
        sandbox: Option<&'a crate::FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, Vec<ReadDirectoryEntry>> {
        self.inner.read_directory(path, sandbox)
    }

    fn walk<'a>(
        &'a self,
        path: &'a PathUri,
        options: WalkOptions,
        sandbox: Option<&'a crate::FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, WalkOutcome> {
        self.inner.walk(path, options, sandbox)
    }

    fn remove<'a>(
        &'a self,
        path: &'a PathUri,
        options: RemoveOptions,
        sandbox: Option<&'a crate::FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        if self.fail_remove {
            return Box::pin(async { Err(io::Error::other("injected remove failure")) });
        }
        self.inner.remove(path, options, sandbox)
    }

    fn copy<'a>(
        &'a self,
        source_path: &'a PathUri,
        destination_path: &'a PathUri,
        options: CopyOptions,
        sandbox: Option<&'a crate::FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        self.inner.copy(source_path, destination_path, options, sandbox)
    }
}

#[tokio::test]
async fn mid_batch_failure_rolls_back() -> io::Result<()> {
    let temp = TempDir::new()?;
    let first = temp.path().join("first.txt");
    let second = temp.path().join("second.txt");
    std::fs::write(&first, b"before")?;
    std::fs::write(&second, b"remove")?;
    let fs = FailingFs::new(None, true);

    let outcome = mutate_batch_on(
        &fs,
        batch(
            vec![
                write(&first, FilePreimage::Exact(b"before".to_vec()), b"after"),
                remove(&second, b"remove"),
            ],
            false,
        ),
    )
    .await;

    assert!(matches!(outcome, FileMutationBatchOutcome::RolledBack { .. }));
    assert_eq!(std::fs::read(first)?, b"before");
    assert_eq!(std::fs::read(second)?, b"remove");
    Ok(())
}

#[tokio::test]
async fn rollback_failure_is_indeterminate() -> io::Result<()> {
    let temp = TempDir::new()?;
    let first = temp.path().join("first.txt");
    let second = temp.path().join("second.txt");
    std::fs::write(&first, b"before")?;
    std::fs::write(&second, b"remove")?;
    let fs = FailingFs::new(Some(2), true);

    let outcome = mutate_batch_on(
        &fs,
        batch(
            vec![
                write(&first, FilePreimage::Exact(b"before".to_vec()), b"after"),
                remove(&second, b"remove"),
            ],
            false,
        ),
    )
    .await;

    match outcome {
        FileMutationBatchOutcome::Indeterminate {
            possibly_mutated_paths,
            ..
        } => assert!(possibly_mutated_paths.contains(&path_uri(&first))),
        other => panic!("expected indeterminate outcome, got {other:?}"),
    }
    assert_eq!(std::fs::read(first)?, b"after");
    assert_eq!(std::fs::read(second)?, b"remove");
    Ok(())
}
