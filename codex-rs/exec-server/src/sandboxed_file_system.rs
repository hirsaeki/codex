use std::future::Future;
#[cfg(windows)]
use std::process::Stdio;
#[cfg(windows)]
use std::sync::Arc;
#[cfg(windows)]
use std::time::Instant;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use codex_exec_server_protocol::JSONRPCErrorError;
use codex_utils_path_uri::PathUri;
use tokio::io;
#[cfg(windows)]
use tokio::io::AsyncBufReadExt;
#[cfg(windows)]
use tokio::io::AsyncWriteExt;
#[cfg(windows)]
use tokio::io::BufReader;
#[cfg(windows)]
use tokio::sync::Mutex;
use tokio_util::io::ReaderStream;

use crate::CapabilityRootsDiscoverParams;
use crate::CapabilityRootsDiscoverResponse;
use crate::CopyOptions;
use crate::CreateDirectoryOptions;
use crate::ExecServerRuntimePaths;
use crate::ExecutorFileSystem;
use crate::ExecutorFileSystemFuture;
use crate::FILE_READ_CHUNK_SIZE;
use crate::FileMetadata;
use crate::FileSystemReadStream;
use crate::FileSystemResult;
use crate::FileSystemSandboxContext;
use crate::GetMetadataOptions;
use crate::ReadDirectoryEntry;
use crate::ReadFileOptions;
use crate::RemoveOptions;
use crate::WalkOptions;
use crate::WalkOutcome;
use crate::WriteFileOptions;
use crate::fs_helper::FsHelperPayload;
use crate::fs_helper::FsHelperRequest;
#[cfg(windows)]
use crate::fs_helper::FsHelperResponse;
use crate::fs_sandbox::FileSystemSandboxRunner;
#[cfg(windows)]
use crate::fs_sandbox::drain_helper_stderr;
#[cfg(windows)]
use crate::fs_sandbox::reap_helper_after_response;
#[cfg(windows)]
use crate::fs_sandbox::spawn_command;
use crate::protocol::CAPABILITY_ROOTS_DISCOVER_METHOD;
use crate::protocol::FS_CANONICALIZE_METHOD;
use crate::protocol::FS_COPY_METHOD;
use crate::protocol::FS_CREATE_DIRECTORY_METHOD;
use crate::protocol::FS_GET_METADATA_METHOD;
use crate::protocol::FS_OPEN_METHOD;
use crate::protocol::FS_READ_DIRECTORY_METHOD;
use crate::protocol::FS_READ_FILE_METHOD;
use crate::protocol::FS_REMOVE_METHOD;
use crate::protocol::FS_WALK_METHOD;
use crate::protocol::FS_WRITE_FILE_METHOD;
use crate::protocol::FsCanonicalizeParams;
use crate::protocol::FsCopyParams;
use crate::protocol::FsCreateDirectoryParams;
use crate::protocol::FsGetMetadataParams;
use crate::protocol::FsReadDirectoryParams;
use crate::protocol::FsReadFileParams;
use crate::protocol::FsRemoveParams;
use crate::protocol::FsWalkParams;
use crate::protocol::FsWriteFileParams;

#[cfg(windows)]
tokio::task_local! {
    static APPLY_PATCH_FS_HELPER: Arc<Mutex<Option<ScopedFsHelper>>>;
}

/// Runs one apply_patch filesystem phase with at most one reusable Windows helper.
///
/// This is deliberately lexical rather than a process-wide pool: the helper is
/// created lazily on the first sandboxed request and is closed before this future
/// returns. Non-Windows callers preserve the existing one-request helper behavior.
pub async fn with_apply_patch_fs_helper_reuse<F>(future: F) -> F::Output
where
    F: Future,
{
    #[cfg(windows)]
    {
        let slot = Arc::new(Mutex::new(None));
        let output = APPLY_PATCH_FS_HELPER.scope(Arc::clone(&slot), future).await;
        let helper = slot.lock().await.take();
        if let Some(helper) = helper
            && let Err(error) = helper.finish().await
        {
            tracing::warn!(%error, "failed to clean up apply_patch filesystem helper");
        }
        output
    }

    #[cfg(not(windows))]
    {
        future.await
    }
}

#[cfg(windows)]
#[derive(Clone, Debug, Eq, PartialEq)]
struct ScopedFsHelperKey {
    runner_id: usize,
    sandbox: FileSystemSandboxContext,
}

#[cfg(windows)]
struct ScopedFsHelper {
    key: ScopedFsHelperKey,
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    stderr: tokio::task::JoinHandle<Result<Vec<u8>, std::io::Error>>,
}

#[cfg(windows)]
impl ScopedFsHelper {
    fn start(
        key: ScopedFsHelperKey,
        command: codex_sandboxing::SandboxExecRequest,
    ) -> FileSystemResult<Self> {
        let mut child = spawn_command(command, Stdio::piped()).map_err(map_sandbox_error)?;
        let stdin = child.stdin.take().ok_or_else(|| {
            io::Error::other("failed to open apply_patch fs helper stdin")
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            io::Error::other("failed to open apply_patch fs helper stdout")
        })?;
        let stderr = drain_helper_stderr(&mut child);
        Ok(Self {
            key,
            child,
            stdin,
            stdout: BufReader::new(stdout),
            stderr,
        })
    }

    async fn run(&mut self, request: FsHelperRequest) -> FileSystemResult<FsHelperPayload> {
        let operation = fs_helper_operation(&request);
        let mut request_json = serde_json::to_vec(&request).map_err(|error| {
            io::Error::other(format!("failed to encode fs sandbox helper message: {error}"))
        })?;
        request_json.push(b'\n');

        let started_at = Instant::now();
        tracing::debug!(operation, "filesystem sandbox helper invocation started");
        let result = async {
            self.stdin.write_all(&request_json).await?;
            self.stdin.flush().await?;

            let mut response = Vec::new();
            let bytes_read = self.stdout.read_until(b'\n', &mut response).await?;
            if bytes_read == 0 {
                return Err(io::Error::other(
                    "fs sandbox helper closed stdout without responding",
                ));
            }
            let response: FsHelperResponse = serde_json::from_slice(&response).map_err(|error| {
                io::Error::other(format!("failed to decode fs sandbox helper message: {error}"))
            })?;
            match response {
                FsHelperResponse::Ok(payload) => Ok(payload),
                FsHelperResponse::Error(error) => Err(map_sandbox_error(error)),
            }
        }
        .await;
        tracing::debug!(
            operation,
            success = result.is_ok(),
            elapsed_ms = started_at.elapsed().as_secs_f64() * 1000.0,
            "filesystem sandbox helper invocation completed"
        );
        result
    }

    async fn finish(mut self) -> FileSystemResult<()> {
        self.stdin.shutdown().await?;
        drop(self.stdin);
        reap_helper_after_response(self.child, self.stderr)
            .await
            .map_err(map_sandbox_error)
    }
}

#[derive(Clone)]
pub struct SandboxedFileSystem {
    sandbox_runner: FileSystemSandboxRunner,
    #[cfg(windows)]
    scoped_runner_id: Arc<()>,
}

impl SandboxedFileSystem {
    #[tracing::instrument(
        name = "capability_roots.discover_v1",
        skip_all,
        fields(root_count = params.roots.len())
    )]
    pub(crate) async fn discover_capability_roots(
        &self,
        params: CapabilityRootsDiscoverParams,
        sandbox: &FileSystemSandboxContext,
    ) -> FileSystemResult<CapabilityRootsDiscoverResponse> {
        self.run_sandboxed(sandbox, FsHelperRequest::DiscoverCapabilityRoots(params))
            .await?
            .expect_capability_roots_discover()
            .map_err(map_sandbox_error)
    }

    pub(crate) async fn open_file_for_read(
        &self,
        path: &PathUri,
        sandbox: Option<&FileSystemSandboxContext>,
    ) -> FileSystemResult<tokio::fs::File> {
        let sandbox = require_platform_sandbox(sandbox)?;
        validate_native_path(path)?;
        let command = self
            .sandbox_runner
            .sandbox_command(sandbox)
            .map_err(map_sandbox_error)?;
        crate::sandboxed_file_open::open(command, path.clone())
            .await
            .map_err(map_sandbox_error)
    }

    pub fn new(runtime_paths: ExecServerRuntimePaths) -> Self {
        Self {
            sandbox_runner: FileSystemSandboxRunner::new(runtime_paths),
            #[cfg(windows)]
            scoped_runner_id: Arc::new(()),
        }
    }

    async fn run_sandboxed(
        &self,
        sandbox: &FileSystemSandboxContext,
        request: FsHelperRequest,
    ) -> FileSystemResult<FsHelperPayload> {
        #[cfg(windows)]
        if let Ok(slot) = APPLY_PATCH_FS_HELPER.try_with(Arc::clone) {
            let key = ScopedFsHelperKey {
                runner_id: Arc::as_ptr(&self.scoped_runner_id) as usize,
                sandbox: sandbox.clone(),
            };
            let mut helper = slot.lock().await;
            match helper.as_mut() {
                Some(helper) if helper.key == key => return helper.run(request).await,
                Some(_) => {}
                None => {
                    let command = self
                        .sandbox_runner
                        .sandbox_command(sandbox)
                        .map_err(map_sandbox_error)?;
                    *helper = Some(ScopedFsHelper::start(key, command)?);
                    return helper
                        .as_mut()
                        .expect("scoped filesystem helper was just installed")
                        .run(request)
                        .await;
                }
            }
        }

        self.sandbox_runner
            .run(sandbox, request)
            .await
            .map_err(map_sandbox_error)
    }
}

impl SandboxedFileSystem {
    async fn canonicalize(
        &self,
        path: &PathUri,
        sandbox: Option<&FileSystemSandboxContext>,
    ) -> FileSystemResult<PathUri> {
        let sandbox = require_platform_sandbox(sandbox)?;
        validate_native_path(path)?;
        let response = self
            .run_sandboxed(
                sandbox,
                FsHelperRequest::Canonicalize(FsCanonicalizeParams {
                    path: path.clone(),
                    sandbox: None,
                }),
            )
            .await?
            .expect_canonicalize()
            .map_err(map_sandbox_error)?;
        Ok(response.path)
    }

    async fn read_file(
        &self,
        path: &PathUri,
        options: ReadFileOptions,
        sandbox: Option<&FileSystemSandboxContext>,
    ) -> FileSystemResult<Vec<u8>> {
        let sandbox = require_platform_sandbox(sandbox)?;
        validate_native_path(path)?;
        let response = self
            .run_sandboxed(
                sandbox,
                FsHelperRequest::ReadFile(FsReadFileParams {
                    path: path.clone(),
                    follow_symlinks: (!options.follow_symlinks).then_some(false),
                    sandbox: None,
                }),
            )
            .await?
            .expect_read_file()
            .map_err(map_sandbox_error)?;
        STANDARD.decode(response.data_base64).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("fs/readFile returned invalid base64 dataBase64: {err}"),
            )
        })
    }

    async fn write_file(
        &self,
        path: &PathUri,
        contents: Vec<u8>,
        options: WriteFileOptions,
        sandbox: Option<&FileSystemSandboxContext>,
    ) -> FileSystemResult<()> {
        let sandbox = require_platform_sandbox(sandbox)?;
        validate_native_path(path)?;
        self.run_sandboxed(
            sandbox,
            FsHelperRequest::WriteFile(FsWriteFileParams {
                path: path.clone(),
                data_base64: STANDARD.encode(contents),
                follow_symlinks: (!options.follow_symlinks).then_some(false),
                sandbox: None,
            }),
        )
        .await?
        .expect_write_file()
        .map_err(map_sandbox_error)?;
        Ok(())
    }

    async fn create_directory(
        &self,
        path: &PathUri,
        options: CreateDirectoryOptions,
        sandbox: Option<&FileSystemSandboxContext>,
    ) -> FileSystemResult<()> {
        let sandbox = require_platform_sandbox(sandbox)?;
        validate_native_path(path)?;
        self.run_sandboxed(
            sandbox,
            FsHelperRequest::CreateDirectory(FsCreateDirectoryParams {
                path: path.clone(),
                recursive: Some(options.recursive),
                follow_symlinks: (!options.follow_symlinks).then_some(false),
                sandbox: None,
            }),
        )
        .await?
        .expect_create_directory()
        .map_err(map_sandbox_error)?;
        Ok(())
    }

    async fn get_metadata(
        &self,
        path: &PathUri,
        options: GetMetadataOptions,
        sandbox: Option<&FileSystemSandboxContext>,
    ) -> FileSystemResult<FileMetadata> {
        let sandbox = require_platform_sandbox(sandbox)?;
        validate_native_path(path)?;
        let response = self
            .run_sandboxed(
                sandbox,
                FsHelperRequest::GetMetadata(FsGetMetadataParams {
                    path: path.clone(),
                    follow_symlinks: (!options.follow_symlinks).then_some(false),
                    sandbox: None,
                }),
            )
            .await?
            .expect_get_metadata()
            .map_err(map_sandbox_error)?;
        Ok(FileMetadata {
            is_directory: response.is_directory,
            is_file: response.is_file,
            is_symlink: response.is_symlink,
            size: response.size,
            created_at_ms: response.created_at_ms,
            modified_at_ms: response.modified_at_ms,
        })
    }

    async fn read_directory(
        &self,
        path: &PathUri,
        sandbox: Option<&FileSystemSandboxContext>,
    ) -> FileSystemResult<Vec<ReadDirectoryEntry>> {
        let sandbox = require_platform_sandbox(sandbox)?;
        validate_native_path(path)?;
        let response = self
            .run_sandboxed(
                sandbox,
                FsHelperRequest::ReadDirectory(FsReadDirectoryParams {
                    path: path.clone(),
                    sandbox: None,
                }),
            )
            .await?
            .expect_read_directory()
            .map_err(map_sandbox_error)?;
        Ok(response
            .entries
            .into_iter()
            .map(|entry| ReadDirectoryEntry {
                file_name: entry.file_name,
                is_directory: entry.is_directory,
                is_file: entry.is_file,
            })
            .collect())
    }

    async fn walk(
        &self,
        path: &PathUri,
        options: WalkOptions,
        sandbox: Option<&FileSystemSandboxContext>,
    ) -> FileSystemResult<WalkOutcome> {
        let sandbox = require_platform_sandbox(sandbox)?;
        validate_native_path(path)?;
        let response = self
            .run_sandboxed(
                sandbox,
                FsHelperRequest::Walk(FsWalkParams {
                    path: path.clone(),
                    options,
                    sandbox: None,
                }),
            )
            .await?
            .expect_walk()
            .map_err(map_sandbox_error)?;
        Ok(response)
    }

    async fn remove(
        &self,
        path: &PathUri,
        remove_options: RemoveOptions,
        sandbox: Option<&FileSystemSandboxContext>,
    ) -> FileSystemResult<()> {
        let sandbox = require_platform_sandbox(sandbox)?;
        validate_native_path(path)?;
        self.run_sandboxed(
            sandbox,
            FsHelperRequest::Remove(FsRemoveParams {
                path: path.clone(),
                recursive: Some(remove_options.recursive),
                force: Some(remove_options.force),
                follow_symlinks: (!remove_options.follow_symlinks).then_some(false),
                sandbox: None,
            }),
        )
        .await?
        .expect_remove()
        .map_err(map_sandbox_error)?;
        Ok(())
    }

    async fn copy(
        &self,
        source_path: &PathUri,
        destination_path: &PathUri,
        options: CopyOptions,
        sandbox: Option<&FileSystemSandboxContext>,
    ) -> FileSystemResult<()> {
        let sandbox = require_platform_sandbox(sandbox)?;
        validate_native_path(source_path)?;
        validate_native_path(destination_path)?;
        self.run_sandboxed(
            sandbox,
            FsHelperRequest::Copy(FsCopyParams {
                source_path: source_path.clone(),
                destination_path: destination_path.clone(),
                recursive: options.recursive,
                sandbox: None,
            }),
        )
        .await?
        .expect_copy()
        .map_err(map_sandbox_error)?;
        Ok(())
    }
}

impl ExecutorFileSystem for SandboxedFileSystem {
    fn canonicalize<'a>(
        &'a self,
        path: &'a PathUri,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, PathUri> {
        Box::pin(SandboxedFileSystem::canonicalize(self, path, sandbox))
    }

    fn read_file<'a>(
        &'a self,
        path: &'a PathUri,
        options: ReadFileOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, Vec<u8>> {
        Box::pin(SandboxedFileSystem::read_file(self, path, options, sandbox))
    }

    fn read_file_stream<'a>(
        &'a self,
        path: &'a PathUri,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileSystemReadStream> {
        Box::pin(async move {
            let file = self.open_file_for_read(path, sandbox).await?;
            Ok(FileSystemReadStream::new(ReaderStream::with_capacity(
                file,
                FILE_READ_CHUNK_SIZE,
            )))
        })
    }

    fn write_file<'a>(
        &'a self,
        path: &'a PathUri,
        contents: Vec<u8>,
        options: WriteFileOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(SandboxedFileSystem::write_file(
            self, path, contents, options, sandbox,
        ))
    }

    fn create_directory<'a>(
        &'a self,
        path: &'a PathUri,
        options: CreateDirectoryOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(SandboxedFileSystem::create_directory(
            self, path, options, sandbox,
        ))
    }

    fn get_metadata<'a>(
        &'a self,
        path: &'a PathUri,
        options: GetMetadataOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileMetadata> {
        Box::pin(SandboxedFileSystem::get_metadata(
            self, path, options, sandbox,
        ))
    }

    fn read_directory<'a>(
        &'a self,
        path: &'a PathUri,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, Vec<ReadDirectoryEntry>> {
        Box::pin(SandboxedFileSystem::read_directory(self, path, sandbox))
    }

    fn walk<'a>(
        &'a self,
        path: &'a PathUri,
        options: WalkOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, WalkOutcome> {
        Box::pin(SandboxedFileSystem::walk(self, path, options, sandbox))
    }

    fn remove<'a>(
        &'a self,
        path: &'a PathUri,
        remove_options: RemoveOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(SandboxedFileSystem::remove(
            self,
            path,
            remove_options,
            sandbox,
        ))
    }

    fn copy<'a>(
        &'a self,
        source_path: &'a PathUri,
        destination_path: &'a PathUri,
        options: CopyOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(SandboxedFileSystem::copy(
            self,
            source_path,
            destination_path,
            options,
            sandbox,
        ))
    }
}

fn validate_native_path(path: &PathUri) -> FileSystemResult<()> {
    path.to_abs_path().map(drop)
}

fn require_platform_sandbox(
    sandbox: Option<&FileSystemSandboxContext>,
) -> FileSystemResult<&FileSystemSandboxContext> {
    sandbox
        .filter(|sandbox| sandbox.should_run_in_sandbox())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "sandboxed filesystem operations require ReadOnly or WorkspaceWrite sandbox policy",
            )
        })
}

#[cfg(windows)]
fn fs_helper_operation(request: &FsHelperRequest) -> &'static str {
    match request {
        FsHelperRequest::DiscoverCapabilityRoots(_) => CAPABILITY_ROOTS_DISCOVER_METHOD,
        FsHelperRequest::Open(_) => FS_OPEN_METHOD,
        FsHelperRequest::ReadFile(_) => FS_READ_FILE_METHOD,
        FsHelperRequest::WriteFile(_) => FS_WRITE_FILE_METHOD,
        FsHelperRequest::CreateDirectory(_) => FS_CREATE_DIRECTORY_METHOD,
        FsHelperRequest::GetMetadata(_) => FS_GET_METADATA_METHOD,
        FsHelperRequest::Canonicalize(_) => FS_CANONICALIZE_METHOD,
        FsHelperRequest::ReadDirectory(_) => FS_READ_DIRECTORY_METHOD,
        FsHelperRequest::Walk(_) => FS_WALK_METHOD,
        FsHelperRequest::Remove(_) => FS_REMOVE_METHOD,
        FsHelperRequest::Copy(_) => FS_COPY_METHOD,
    }
}

fn map_sandbox_error(error: JSONRPCErrorError) -> io::Error {
    match error.code {
        -32004 => io::Error::new(io::ErrorKind::NotFound, error.message),
        -32600 => io::Error::new(io::ErrorKind::InvalidInput, error.message),
        _ => io::Error::other(error.message),
    }
}

#[cfg(all(test, any(unix, windows)))]
#[path = "sandboxed_file_system_path_uri_tests.rs"]
mod path_uri_tests;
