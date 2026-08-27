from pathlib import Path


def read(path: str) -> str:
    return Path(path).read_text()


def write(path: str, text: str) -> None:
    Path(path).write_text(text)


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise RuntimeError(f"{label}: expected one match, got {count}")
    return text.replace(old, new, 1)


# file-system: keep the legacy capability shape, but bind no-follow policy to the batch.
path = "codex-rs/file-system/src/lib.rs"
text = read(path)
text = replace_once(
    text,
    "pub struct FileMutationBatch {\n    pub mutations: Vec<FileMutation>,\n}",
    "pub struct FileMutationBatch {\n    pub mutations: Vec<FileMutation>,\n    /// Whether every filesystem operation in this logical batch may traverse symlinks.\n    pub follow_symlinks: bool,\n}",
    "FileMutationBatch follow_symlinks",
)
write(path, text)

# Protocol: current protocol conventions use optional followSymlinks for backward compatibility.
path = "codex-rs/exec-server-protocol/src/protocol.rs"
text = read(path)
text = replace_once(
    text,
    "use codex_file_system::FileSystemSandboxContext;",
    "use codex_file_system::FileMutation;\nuse codex_file_system::FileMutationBatchOutcome;\nuse codex_file_system::FilePreimage;\nuse codex_file_system::FileSystemSandboxContext;",
    "protocol mutation imports",
)
text = replace_once(
    text,
    'pub const FS_COPY_METHOD: &str = "fs/copy";\n',
    'pub const FS_COPY_METHOD: &str = "fs/copy";\npub const FS_MUTATE_BATCH_METHOD: &str = "fs/mutateBatch";\npub const MAX_FS_MUTATE_BATCH_OPERATIONS: usize = 10_000;\npub const MAX_FS_MUTATE_BATCH_DECODED_BYTES: usize = 512 * 1024 * 1024;\n',
    "protocol mutate constant",
)
marker = "#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]\n#[serde(rename_all = \"camelCase\")]\npub struct FsCopyResponse {}\n"
insert = marker + r'''

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsMutateBatchParams {
    pub mutations: Vec<FsMutation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_symlinks: Option<bool>,
    pub sandbox: Option<FileSystemSandboxContext>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsMutateBatchResponse {
    pub outcome: FsMutationBatchOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FsMutation {
    Write {
        path: PathUri,
        expected: FsFilePreimage,
        contents: ByteChunk,
    },
    Remove {
        path: PathUri,
        expected: ByteChunk,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "contents", rename_all = "camelCase")]
pub enum FsFilePreimage {
    Missing,
    Exact(ByteChunk),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum FsMutationBatchOutcome {
    Committed,
    Rejected {
        error: String,
    },
    RolledBack {
        error: String,
    },
    Indeterminate {
        error: String,
        possibly_mutated_paths: Vec<PathUri>,
    },
}

impl From<FileMutation> for FsMutation {
    fn from(mutation: FileMutation) -> Self {
        match mutation {
            FileMutation::Write {
                path,
                expected,
                contents,
            } => Self::Write {
                path,
                expected: match expected {
                    FilePreimage::Missing => FsFilePreimage::Missing,
                    FilePreimage::Exact(contents) => FsFilePreimage::Exact(contents.into()),
                },
                contents: contents.into(),
            },
            FileMutation::Remove { path, expected } => Self::Remove {
                path,
                expected: expected.into(),
            },
        }
    }
}

impl From<FsMutation> for FileMutation {
    fn from(mutation: FsMutation) -> Self {
        match mutation {
            FsMutation::Write {
                path,
                expected,
                contents,
            } => Self::Write {
                path,
                expected: match expected {
                    FsFilePreimage::Missing => FilePreimage::Missing,
                    FsFilePreimage::Exact(contents) => FilePreimage::Exact(contents.into_inner()),
                },
                contents: contents.into_inner(),
            },
            FsMutation::Remove { path, expected } => Self::Remove {
                path,
                expected: expected.into_inner(),
            },
        }
    }
}

impl From<FileMutationBatchOutcome> for FsMutationBatchOutcome {
    fn from(outcome: FileMutationBatchOutcome) -> Self {
        match outcome {
            FileMutationBatchOutcome::Committed => Self::Committed,
            FileMutationBatchOutcome::Rejected { error } => Self::Rejected { error },
            FileMutationBatchOutcome::RolledBack { error } => Self::RolledBack { error },
            FileMutationBatchOutcome::Indeterminate {
                error,
                possibly_mutated_paths,
            } => Self::Indeterminate {
                error,
                possibly_mutated_paths,
            },
        }
    }
}

impl From<FsMutationBatchOutcome> for FileMutationBatchOutcome {
    fn from(outcome: FsMutationBatchOutcome) -> Self {
        match outcome {
            FsMutationBatchOutcome::Committed => Self::Committed,
            FsMutationBatchOutcome::Rejected { error } => Self::Rejected { error },
            FsMutationBatchOutcome::RolledBack { error } => Self::RolledBack { error },
            FsMutationBatchOutcome::Indeterminate {
                error,
                possibly_mutated_paths,
            } => Self::Indeterminate {
                error,
                possibly_mutated_paths,
            },
        }
    }
}
'''
text = replace_once(text, marker, insert, "protocol mutate types")
write(path, text)

# Server protocol endpoint: limits are checked before converting to internal bytes.
path = "codex-rs/exec-server/src/server/file_system_handler.rs"
text = read(path)
text = replace_once(
    text,
    "use crate::ExecutorFileSystem;\n",
    "use crate::ExecutorFileSystem;\nuse crate::FileMutationBatch;\nuse crate::FileMutationBatchOutcome;\n",
    "handler internal batch imports",
)
text = replace_once(
    text,
    "use crate::protocol::FS_READ_DIRECTORY_METHOD;\n",
    "use crate::protocol::FS_READ_DIRECTORY_METHOD;\nuse crate::protocol::MAX_FS_MUTATE_BATCH_DECODED_BYTES;\nuse crate::protocol::MAX_FS_MUTATE_BATCH_OPERATIONS;\n",
    "handler limit imports",
)
text = replace_once(
    text,
    "use crate::protocol::FsGetMetadataResponse;\n",
    "use crate::protocol::FsGetMetadataResponse;\nuse crate::protocol::FsFilePreimage;\nuse crate::protocol::FsMutateBatchParams;\nuse crate::protocol::FsMutateBatchResponse;\nuse crate::protocol::FsMutation;\nuse crate::protocol::FsMutationBatchOutcome;\n",
    "handler protocol batch imports",
)
copy_method = '''    pub(crate) async fn copy(
        &self,
        params: FsCopyParams,
    ) -> Result<FsCopyResponse, JSONRPCErrorError> {
        self.file_system
            .copy(
                &params.source_path,
                &params.destination_path,
                CopyOptions {
                    recursive: params.recursive,
                },
                params.sandbox.as_ref(),
            )
            .await
            .map_err(map_fs_error)?;
        Ok(FsCopyResponse {})
    }
'''
mutate_method = copy_method + '''
    pub(crate) async fn mutate_batch(
        &self,
        params: FsMutateBatchParams,
    ) -> Result<FsMutateBatchResponse, JSONRPCErrorError> {
        mutate_batch(&self.file_system, params).await
    }
'''
text = replace_once(text, copy_method, mutate_method, "handler mutate method")
end_impl = "}\n\nfn validate_file_read_handle_id(handle_id: &str) -> Result<(), JSONRPCErrorError> {\n"
free_fn = r''' }

pub(crate) async fn mutate_batch(
    file_system: &dyn ExecutorFileSystem,
    params: FsMutateBatchParams,
) -> Result<FsMutateBatchResponse, JSONRPCErrorError> {
    if params.mutations.len() > MAX_FS_MUTATE_BATCH_OPERATIONS {
        return Err(invalid_request(format!(
            "filesystem mutation batch has {} operations; limit is {MAX_FS_MUTATE_BATCH_OPERATIONS}",
            params.mutations.len()
        )));
    }
    let decoded_bytes = params.mutations.iter().try_fold(0usize, |total, mutation| {
        let bytes = match mutation {
            FsMutation::Write {
                expected, contents, ..
            } => {
                contents.0.len()
                    + match expected {
                        FsFilePreimage::Missing => 0,
                        FsFilePreimage::Exact(contents) => contents.0.len(),
                    }
            }
            FsMutation::Remove { expected, .. } => expected.0.len(),
        };
        total.checked_add(bytes)
    });
    if decoded_bytes.is_none_or(|bytes| bytes > MAX_FS_MUTATE_BATCH_DECODED_BYTES) {
        return Err(invalid_request(format!(
            "filesystem mutation batch decoded bytes exceed limit {MAX_FS_MUTATE_BATCH_DECODED_BYTES}"
        )));
    }
    let batch = FileMutationBatch {
        mutations: params.mutations.into_iter().map(Into::into).collect(),
        follow_symlinks: params.follow_symlinks.unwrap_or(true),
    };
    let outcome = file_system
        .mutate_batch(batch, params.sandbox.as_ref())
        .await
        .map_err(map_fs_error)?;
    Ok(FsMutateBatchResponse {
        outcome: outcome.into(),
    })
}

fn validate_file_read_handle_id(handle_id: &str) -> Result<(), JSONRPCErrorError> {
'''
# The leading space is stripped deliberately so the replacement matches the impl close.
free_fn = free_fn[1:]
text = replace_once(text, end_impl, free_fn, "handler mutate free function")
write(path, text)

# apply_patch: prepare only at execution time with the runtime-selected options.
path = "codex-rs/apply-patch/src/lib.rs"
text = read(path)
text = replace_once(text, "mod parser;\n", "mod parser;\nmod prepared;\n", "prepared module")
text = replace_once(
    text,
    "use codex_exec_server::FileSystemSandboxContext;\n",
    "use codex_exec_server::FileMutationBatchOutcome;\nuse codex_exec_server::FileSystemSandboxContext;\n",
    "apply batch outcome import",
)
text = replace_once(
    text,
    '''    /// A patch path could not be resolved as a path URI.
    #[error(transparent)]
    PathUri(#[from] PathUriParseError),
''',
    '''    /// A patch path could not be resolved as a path URI.
    #[error(transparent)]
    PathUri(#[from] PathUriParseError),
    /// A logical mutation batch was rejected or rolled back.
    #[error("filesystem mutation batch failed: {0}")]
    BatchMutation(String),
    /// The batch could not prove the final filesystem state.
    #[error("filesystem mutation batch left an indeterminate state: {0}")]
    Indeterminate(String),
''',
    "apply batch errors",
)
text = replace_once(
    text,
    '''    pub fn into_parts(self) -> (ApplyPatchError, AppliedPatchDelta) {
        (self.error, self.delta)
    }
''',
    '''    pub fn into_parts(self) -> (ApplyPatchError, AppliedPatchDelta) {
        (self.error, self.delta)
    }

    pub fn is_indeterminate(&self) -> bool {
        matches!(self.error, ApplyPatchError::Indeterminate(_))
    }
''',
    "apply failure indeterminate",
)
old_tail = '''    apply_hunks_with_options(&hunks, options, cwd, stdout, stderr, fs, sandbox).await
}

/// Applies hunks and continues to update stdout/stderr
'''
new_tail = r'''    if hunks.is_empty() {
        return apply_hunks_with_options(&hunks, options, cwd, stdout, stderr, fs, sandbox).await;
    }

    let prepared = match prepared::prepare_hunks(&hunks, options, cwd, fs, sandbox).await {
        Ok(prepared) => prepared,
        Err(error) => return report_preparation_failure(error, stderr),
    };
    apply_prepared_or_legacy(
        &prepared,
        &hunks,
        options,
        cwd,
        stdout,
        stderr,
        fs,
        sandbox,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn apply_prepared_or_legacy(
    prepared: &prepared::PreparedPatch,
    hunks: &[Hunk],
    options: ApplyPatchOptions,
    cwd: &PathUri,
    stdout: &mut impl std::io::Write,
    stderr: &mut impl std::io::Write,
    fs: &dyn ExecutorFileSystem,
    sandbox: Option<&FileSystemSandboxContext>,
) -> Result<AppliedPatchDelta, ApplyPatchFailure> {
    let outcome = match fs.mutate_batch(prepared.batch.clone(), sandbox).await {
        Ok(outcome) => outcome,
        Err(error) if error.kind() == io::ErrorKind::Unsupported => {
            return apply_hunks_with_options(hunks, options, cwd, stdout, stderr, fs, sandbox).await;
        }
        Err(error) => return report_preparation_failure(error.into(), stderr),
    };

    match outcome {
        FileMutationBatchOutcome::Committed => {
            print_summary(&prepared.affected, stdout).map_err(|error| {
                ApplyPatchFailure::new(ApplyPatchError::from(error), prepared.delta.clone())
            })?;
            Ok(prepared.delta.clone())
        }
        FileMutationBatchOutcome::Rejected { error }
        | FileMutationBatchOutcome::RolledBack { error } => {
            report_preparation_failure(ApplyPatchError::BatchMutation(error), stderr)
        }
        FileMutationBatchOutcome::Indeterminate {
            error,
            possibly_mutated_paths,
        } => {
            let mut delta = prepared.delta.clone();
            delta.exact = false;
            delta.changes.retain(|change| {
                possibly_mutated_paths.contains(&change.path)
                    || matches!(
                        &change.change,
                        AppliedPatchFileChange::Update {
                            move_path: Some(path),
                            ..
                        } if possibly_mutated_paths.contains(path)
                    )
            });
            let error = ApplyPatchError::Indeterminate(error);
            writeln!(stderr, "{error}")
                .map_err(ApplyPatchError::from)
                .map_err(ApplyPatchFailure::without_delta)?;
            Err(ApplyPatchFailure::new(error, delta))
        }
    }
}

fn report_preparation_failure<T>(
    error: ApplyPatchError,
    stderr: &mut impl std::io::Write,
) -> Result<T, ApplyPatchFailure> {
    writeln!(stderr, "{error}")
        .map_err(ApplyPatchError::from)
        .map_err(ApplyPatchFailure::without_delta)?;
    Err(ApplyPatchFailure::without_delta(error))
}

/// Applies hunks and continues to update stdout/stderr
'''
text = replace_once(text, old_tail, new_tail, "execution-time preparation")
text = replace_once(
    text,
    "pub struct AffectedPaths {\n",
    "#[derive(Clone, Debug, Default, PartialEq)]\npub struct AffectedPaths {\n",
    "AffectedPaths derives",
)
write(path, text)

# Remote mutation is capability-by-method: older servers safely fall back on method-not-found.
path = "codex-rs/exec-server/src/remote_file_system.rs"
text = read(path)
capability = '''        if !client
            .environment_info()
            .await
            .map_err(map_remote_error)?
            .capabilities
            .file_system_batch_mutation
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "remote exec-server does not support filesystem mutation batches",
            ));
        }
'''
if capability in text:
    text = text.replace(capability, "", 1)
text = replace_once(
    text,
    "        let possibly_mutated_paths = batch\n",
    "        let follow_symlinks = batch.follow_symlinks;\n        let possibly_mutated_paths = batch\n",
    "remote capture follow",
)
text = replace_once(
    text,
    '''            .fs_mutate_batch(FsMutateBatchParams {
                mutations: batch.mutations.into_iter().map(Into::into).collect(),
                sandbox: remote_sandbox_context(sandbox),
            })
''',
    '''            .fs_mutate_batch(FsMutateBatchParams {
                mutations: batch.mutations.into_iter().map(Into::into).collect(),
                follow_symlinks: (!follow_symlinks).then_some(false),
                sandbox: remote_sandbox_context(sandbox),
            })
''',
    "remote follow protocol",
)
write(path, text)

# Sandboxed local mutation must be one helper request, carrying the same no-follow policy.
path = "codex-rs/exec-server/src/sandboxed_file_system.rs"
text = read(path)
text = replace_once(
    text,
    "        let sandbox = require_platform_sandbox(sandbox)?;\n        let possibly_mutated_paths = batch\n",
    "        let sandbox = require_platform_sandbox(sandbox)?;\n        let follow_symlinks = batch.follow_symlinks;\n        let possibly_mutated_paths = batch\n",
    "sandbox capture follow",
)
text = replace_once(
    text,
    '''                FsHelperRequest::MutateBatch(FsMutateBatchParams {
                    mutations: batch.mutations.into_iter().map(Into::into).collect(),
                    sandbox: None,
                }),
''',
    '''                FsHelperRequest::MutateBatch(FsMutateBatchParams {
                    mutations: batch.mutations.into_iter().map(Into::into).collect(),
                    follow_symlinks: (!follow_symlinks).then_some(false),
                    sandbox: None,
                }),
''',
    "sandbox follow protocol",
)
write(path, text)

# Direct batches run in an owned task so caller cancellation cannot stop rollback halfway through.
path = "codex-rs/exec-server/src/local_file_system.rs"
text = read(path)
old = '''        // Once a spawn_blocking task starts, dropping this future does not cancel the mutation.
        // A join failure therefore cannot prove that the filesystem is unchanged.
        match tokio::task::spawn_blocking(move || crate::file_mutation_batch::mutate_batch(batch))
            .await
        {
'''
new = '''        // Once spawned, dropping this future does not cancel the mutation or its rollback.
        // A join failure therefore cannot prove that the filesystem is unchanged.
        match tokio::spawn(async move { crate::file_mutation_batch::mutate_batch(batch).await }).await {
'''
text = replace_once(text, old, new, "direct async batch task")
write(path, text)
