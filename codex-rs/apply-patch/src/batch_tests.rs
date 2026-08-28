use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use anyhow::Result;
use codex_exec_server::CopyOptions;
use codex_exec_server::CreateDirectoryOptions;
use codex_exec_server::ExecutorFileSystem;
use codex_exec_server::ExecutorFileSystemFuture;
use codex_exec_server::FileMetadata;
use codex_exec_server::FileMutationBatch;
use codex_exec_server::FileMutationBatchOutcome;
use codex_exec_server::FileSystemReadStream;
use codex_exec_server::FileSystemSandboxContext;
use codex_exec_server::GetMetadataOptions;
use codex_exec_server::LOCAL_FS;
use codex_exec_server::ReadDirectoryEntry;
use codex_exec_server::ReadFileOptions;
use codex_exec_server::RemoveOptions;
use codex_exec_server::WalkOptions;
use codex_exec_server::WalkOutcome;
use codex_exec_server::WriteFileOptions;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::*;

#[derive(Clone)]
enum BatchBehavior {
    Delegate,
    Unsupported,
    Fixed(FileMutationBatchOutcome),
}

struct RecordingFileSystem {
    inner: Arc<dyn ExecutorFileSystem>,
    behavior: BatchBehavior,
    batch_calls: AtomicUsize,
    write_calls: AtomicUsize,
    remove_calls: AtomicUsize,
    last_batch: Mutex<Option<FileMutationBatch>>,
}

impl RecordingFileSystem {
    fn new(behavior: BatchBehavior) -> Self {
        Self {
            inner: Arc::clone(&*LOCAL_FS),
            behavior,
            batch_calls: AtomicUsize::new(0),
            write_calls: AtomicUsize::new(0),
            remove_calls: AtomicUsize::new(0),
            last_batch: Mutex::new(None),
        }
    }

    fn batch_calls(&self) -> usize {
        self.batch_calls.load(Ordering::Relaxed)
    }

    fn write_calls(&self) -> usize {
        self.write_calls.load(Ordering::Relaxed)
    }

    fn remove_calls(&self) -> usize {
        self.remove_calls.load(Ordering::Relaxed)
    }

    fn last_batch(&self) -> FileMutationBatch {
        self.last_batch
            .lock()
            .expect("last batch mutex")
            .clone()
            .expect("batch should have been recorded")
    }
}

impl ExecutorFileSystem for RecordingFileSystem {
    fn canonicalize<'a>(
        &'a self,
        path: &'a PathUri,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, PathUri> {
        self.inner.canonicalize(path, sandbox)
    }

    fn read_file<'a>(
        &'a self,
        path: &'a PathUri,
        options: ReadFileOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, Vec<u8>> {
        self.inner.read_file(path, options, sandbox)
    }

    fn read_file_stream<'a>(
        &'a self,
        path: &'a PathUri,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileSystemReadStream> {
        self.inner.read_file_stream(path, sandbox)
    }

    fn write_file<'a>(
        &'a self,
        path: &'a PathUri,
        contents: Vec<u8>,
        options: WriteFileOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        self.write_calls.fetch_add(1, Ordering::Relaxed);
        self.inner.write_file(path, contents, options, sandbox)
    }

    fn mutate_batch<'a>(
        &'a self,
        batch: FileMutationBatch,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileMutationBatchOutcome> {
        self.batch_calls.fetch_add(1, Ordering::Relaxed);
        *self.last_batch.lock().expect("last batch mutex") = Some(batch.clone());
        match &self.behavior {
            BatchBehavior::Delegate => self.inner.mutate_batch(batch, sandbox),
            BatchBehavior::Unsupported => Box::pin(async {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "test backend does not support batches",
                ))
            }),
            BatchBehavior::Fixed(outcome) => {
                let outcome = outcome.clone();
                Box::pin(async move { Ok(outcome) })
            }
        }
    }

    fn create_directory<'a>(
        &'a self,
        path: &'a PathUri,
        options: CreateDirectoryOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        self.inner.create_directory(path, options, sandbox)
    }

    fn get_metadata<'a>(
        &'a self,
        path: &'a PathUri,
        options: GetMetadataOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileMetadata> {
        self.inner.get_metadata(path, options, sandbox)
    }

    fn read_directory<'a>(
        &'a self,
        path: &'a PathUri,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, Vec<ReadDirectoryEntry>> {
        self.inner.read_directory(path, sandbox)
    }

    fn walk<'a>(
        &'a self,
        path: &'a PathUri,
        options: WalkOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, WalkOutcome> {
        self.inner.walk(path, options, sandbox)
    }

    fn remove<'a>(
        &'a self,
        path: &'a PathUri,
        options: RemoveOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        self.remove_calls.fetch_add(1, Ordering::Relaxed);
        self.inner.remove(path, options, sandbox)
    }

    fn copy<'a>(
        &'a self,
        source_path: &'a PathUri,
        destination_path: &'a PathUri,
        options: CopyOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        self.inner
            .copy(source_path, destination_path, options, sandbox)
    }
}

fn cwd(temp: &TempDir) -> Result<PathUri> {
    Ok(PathUri::from_host_native_path(temp.path())?)
}

#[tokio::test]
async fn mutation_phase_uses_one_batch_request_for_multiple_files() -> Result<()> {
    let temp = TempDir::new()?;
    std::fs::write(temp.path().join("modify.txt"), "line1\nline2\n")?;
    std::fs::write(temp.path().join("delete.txt"), "obsolete\n")?;
    let fs = RecordingFileSystem::new(BatchBehavior::Delegate);
    let patch = "*** Begin Patch\n*** Add File: nested/new.txt\n+created\n*** Delete File: delete.txt\n*** Update File: modify.txt\n@@\n-line2\n+changed\n*** End Patch";
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let delta = apply_patch_with_options(
        patch,
        ApplyPatchOptions::default(),
        &cwd(&temp)?,
        &mut stdout,
        &mut stderr,
        &fs,
        None,
    )
    .await
    .expect("batch apply should succeed");

    assert_eq!(fs.batch_calls(), 1);
    assert_eq!(fs.write_calls(), 0);
    assert_eq!(fs.remove_calls(), 0);
    assert_eq!(fs.last_batch().mutations.len(), 3);
    assert!(delta.is_exact());
    assert_eq!(delta.changes().len(), 3);
    assert_eq!(std::fs::read_to_string(temp.path().join("nested/new.txt"))?, "created\n");
    assert_eq!(std::fs::read_to_string(temp.path().join("modify.txt"))?, "line1\nchanged\n");
    assert!(!temp.path().join("delete.txt").exists());
    assert_eq!(String::from_utf8(stderr)?, "");
    assert_eq!(
        String::from_utf8(stdout)?,
        "Success. Updated the following files:\nA nested/new.txt\nM modify.txt\nD delete.txt\n"
    );
    Ok(())
}

#[tokio::test]
async fn unsupported_batch_falls_back_to_sequential_mutation() -> Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("file.txt");
    std::fs::write(&path, "old\n")?;
    let fs = RecordingFileSystem::new(BatchBehavior::Unsupported);
    let patch = "*** Begin Patch\n*** Update File: file.txt\n@@\n-old\n+new\n*** End Patch";
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let delta = apply_patch_with_options(
        patch,
        ApplyPatchOptions::default(),
        &cwd(&temp)?,
        &mut stdout,
        &mut stderr,
        &fs,
        None,
    )
    .await
    .expect("legacy fallback should succeed");

    assert_eq!(fs.batch_calls(), 1);
    assert_eq!(fs.write_calls(), 1);
    assert_eq!(fs.remove_calls(), 0);
    assert!(delta.is_exact());
    assert_eq!(std::fs::read_to_string(path)?, "new\n");
    Ok(())
}

#[tokio::test]
async fn rejected_and_rolled_back_batches_report_no_committed_delta() -> Result<()> {
    for outcome in [
        FileMutationBatchOutcome::Rejected {
            error: "stale preimage".to_string(),
        },
        FileMutationBatchOutcome::RolledBack {
            error: "write failed".to_string(),
        },
    ] {
        let temp = TempDir::new()?;
        let path = temp.path().join("file.txt");
        std::fs::write(&path, "old\n")?;
        let fs = RecordingFileSystem::new(BatchBehavior::Fixed(outcome));
        let patch = "*** Begin Patch\n*** Update File: file.txt\n@@\n-old\n+new\n*** End Patch";
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let failure = apply_patch_with_options(
            patch,
            ApplyPatchOptions::default(),
            &cwd(&temp)?,
            &mut stdout,
            &mut stderr,
            &fs,
            None,
        )
        .await
        .expect_err("batch should fail");

        assert!(failure.delta().is_exact());
        assert!(failure.delta().is_empty());
        assert_eq!(std::fs::read_to_string(path)?, "old\n");
        assert!(stdout.is_empty());
        assert!(!stderr.is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn indeterminate_batch_marks_possibly_mutated_delta_inexact() -> Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("new.txt");
    let path_uri = PathUri::from_host_native_path(&path)?;
    let fs = RecordingFileSystem::new(BatchBehavior::Fixed(
        FileMutationBatchOutcome::Indeterminate {
            error: "connection lost".to_string(),
            possibly_mutated_paths: vec![path_uri],
        },
    ));
    let patch = "*** Begin Patch\n*** Add File: new.txt\n+new\n*** End Patch";
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let failure = apply_patch_with_options(
        patch,
        ApplyPatchOptions::default(),
        &cwd(&temp)?,
        &mut stdout,
        &mut stderr,
        &fs,
        None,
    )
    .await
    .expect_err("batch should be indeterminate");

    assert!(failure.is_indeterminate());
    assert!(!failure.delta().is_exact());
    assert_eq!(failure.delta().changes().len(), 1);
    assert!(stdout.is_empty());
    assert!(String::from_utf8(stderr)?.contains("indeterminate state"));
    Ok(())
}

#[tokio::test]
async fn net_no_op_is_sent_as_an_empty_batch_without_writes() -> Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("same.txt");
    std::fs::write(&path, "same\n")?;
    let fs = RecordingFileSystem::new(BatchBehavior::Delegate);
    let patch = "*** Begin Patch\n*** Add File: same.txt\n+same\n*** End Patch";
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let delta = apply_patch_with_options(
        patch,
        ApplyPatchOptions::default(),
        &cwd(&temp)?,
        &mut stdout,
        &mut stderr,
        &fs,
        None,
    )
    .await
    .expect("no-op batch should succeed");

    assert_eq!(fs.batch_calls(), 1);
    assert!(fs.last_batch().mutations.is_empty());
    assert_eq!(fs.write_calls(), 0);
    assert_eq!(std::fs::read_to_string(path)?, "same\n");
    assert!(delta.is_exact());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn preparation_uses_runtime_follow_symlinks_and_preserves_delta_exactness() -> Result<()> {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new()?;
    let real = temp.path().join("real.txt");
    let link = temp.path().join("link.txt");
    std::fs::write(&real, "old\n")?;
    symlink(&real, &link)?;
    let patch = "*** Begin Patch\n*** Update File: link.txt\n@@\n-old\n+new\n*** End Patch";

    let no_follow_fs = RecordingFileSystem::new(BatchBehavior::Delegate);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let failure = apply_patch_with_options(
        patch,
        ApplyPatchOptions {
            update_file_mode: ApplyPatchFileUpdateMode::PreserveLineEndings,
            follow_symlinks: false,
        },
        &cwd(&temp)?,
        &mut stdout,
        &mut stderr,
        &no_follow_fs,
        None,
    )
    .await
    .expect_err("no-follow preparation should reject the symlink");
    assert!(failure.delta().is_empty());
    assert_eq!(no_follow_fs.batch_calls(), 0);
    assert_eq!(std::fs::read_to_string(&real)?, "old\n");

    let follow_fs = RecordingFileSystem::new(BatchBehavior::Delegate);
    stdout.clear();
    stderr.clear();
    let delta = apply_patch_with_options(
        patch,
        ApplyPatchOptions {
            update_file_mode: ApplyPatchFileUpdateMode::PreserveLineEndings,
            follow_symlinks: true,
        },
        &cwd(&temp)?,
        &mut stdout,
        &mut stderr,
        &follow_fs,
        None,
    )
    .await
    .expect("follow-symlinks mode should keep the supported behavior");
    assert_eq!(follow_fs.batch_calls(), 1);
    assert!(!delta.is_exact());
    assert_eq!(std::fs::read_to_string(real)?, "new\n");
    Ok(())
}
