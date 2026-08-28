from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise RuntimeError(f"{label}: expected one match, got {count}")
    return text.replace(old, new, 1)


# Add stderr checkpoints inside the helper-side transaction engine. The sandbox runner captures
# helper stderr, so the final checkpoint in an Indeterminate error identifies the abort boundary.
path = Path("codex-rs/exec-server/src/file_mutation_batch.rs")
text = path.read_text()
text = replace_once(
    text,
    "pub(crate) async fn mutate_batch(batch: FileMutationBatch) -> FileMutationBatchOutcome {\n    mutate_batch_on(&DirectFileSystem, batch).await\n}",
    '''pub(crate) async fn mutate_batch(batch: FileMutationBatch) -> FileMutationBatchOutcome {
    eprintln!("BATCHDBG enter ops={} follow={}", batch.mutations.len(), batch.follow_symlinks);
    let outcome = mutate_batch_on(&DirectFileSystem, batch).await;
    eprintln!("BATCHDBG exit outcome={outcome:?}");
    outcome
}''',
    "mutate_batch entry",
)
text = replace_once(
    text,
    "    let planned = match preflight(file_system, batch).await {\n        Ok(planned) => planned,",
    '''    eprintln!("BATCHDBG preflight:start");
    let planned = match preflight(file_system, batch).await {
        Ok(planned) => {
            eprintln!("BATCHDBG preflight:ok count={}", planned.len());
            planned
        },''',
    "preflight boundary",
)
text = replace_once(
    text,
    "        let preimage = snapshot(file_system, path, batch.follow_symlinks).await?;",
    '''        eprintln!("BATCHDBG preflight:snapshot:start {}", path.inferred_native_path_string());
        let preimage = snapshot(file_system, path, batch.follow_symlinks).await?;
        eprintln!("BATCHDBG preflight:snapshot:ok {}", path.inferred_native_path_string());''',
    "preflight snapshot",
)
text = replace_once(
    text,
    ") -> io::Result<Snapshot> {\n    let metadata = match file_system",
    ''') -> io::Result<Snapshot> {
    eprintln!("BATCHDBG snapshot:start {} follow={follow_symlinks}", path.inferred_native_path_string());
    let metadata = match file_system''',
    "snapshot entry",
)
text = replace_once(
    text,
    "    };\n    if !metadata.is_file {",
    '''    };
    eprintln!("BATCHDBG snapshot:metadata-ok {} file={} dir={}", path.inferred_native_path_string(), metadata.is_file, metadata.is_directory);
    if !metadata.is_file {''',
    "snapshot metadata",
)
text = replace_once(
    text,
    "    let contents = file_system\n        .read_file(",
    '''    eprintln!("BATCHDBG snapshot:read:start {}", path.inferred_native_path_string());
    let contents = file_system
        .read_file(''',
    "snapshot read start",
)
text = replace_once(
    text,
    "        .await?;\n    Ok(Snapshot::Exact(contents))",
    '''        .await?;
    eprintln!("BATCHDBG snapshot:read:ok {} bytes={}", path.inferred_native_path_string(), contents.len());
    Ok(Snapshot::Exact(contents))''',
    "snapshot read done",
)
text = replace_once(
    text,
    "    let path = mutation_path(&planned.mutation).clone();\n    if matches!(planned.mutation, FileMutation::Write { .. }) {",
    '''    let path = mutation_path(&planned.mutation).clone();
    eprintln!("BATCHDBG apply:start {}", path.inferred_native_path_string());
    if matches!(planned.mutation, FileMutation::Write { .. }) {''',
    "apply entry",
)
text = replace_once(
    text,
    ") -> io::Result<()> {\n    let native = path.to_abs_path()?;",
    ''') -> io::Result<()> {
    eprintln!("BATCHDBG parents:start {} follow={follow_symlinks}", path.inferred_native_path_string());
    let native = path.to_abs_path()?;''',
    "parents entry",
)
text = replace_once(
    text,
    "    for path in missing.into_iter().rev() {\n        let path_uri = PathUri::from_host_native_path(&path)?;",
    '''    for path in missing.into_iter().rev() {
        eprintln!("BATCHDBG parents:create:start {}", path.display());
        let path_uri = PathUri::from_host_native_path(&path)?;''',
    "parent create start",
)
text = replace_once(
    text,
    "                let identity = directory_identity(&path_uri, follow_symlinks).await?;",
    '''                eprintln!("BATCHDBG parents:create:ok {}", path.display());
                eprintln!("BATCHDBG parents:identity:start {}", path.display());
                let identity = directory_identity(&path_uri, follow_symlinks).await?;
                eprintln!("BATCHDBG parents:identity:ok {}", path.display());''',
    "parent identity boundary",
)
text = replace_once(
    text,
    "    let intended = intended_postimage(&planned.mutation);\n    let operation = match &planned.mutation {",
    '''    let intended = intended_postimage(&planned.mutation);
    eprintln!("BATCHDBG mutate:start {}", path.inferred_native_path_string());
    let operation = match &planned.mutation {''',
    "mutation start",
)
text = replace_once(
    text,
    "    let observed = snapshot(file_system, &path, follow_symlinks).await;",
    '''    eprintln!("BATCHDBG mutate:returned {} ok={}", path.inferred_native_path_string(), operation.is_ok());
    let observed = snapshot(file_system, &path, follow_symlinks).await;
    eprintln!("BATCHDBG post-snapshot:returned {} ok={}", path.inferred_native_path_string(), observed.is_ok());''',
    "mutation returned",
)
path.write_text(text)

# Add a focused sandbox-only matrix. Each helper crash is converted to Indeterminate by the
# SandboxedFileSystem layer, so all cases can run in one test process and one build.
path = Path("codex-rs/exec-server/tests/file_system/shared.rs")
text = path.read_text()
anchor = '''#[test_case(FileSystemImplementation::Local ; "local")]
#[test_case(FileSystemImplementation::Remote ; "remote")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_system_get_metadata_reports_files_and_directories(
'''
if text.count(anchor) != 1:
    raise RuntimeError(f"diagnostic test anchor mismatch: {text.count(anchor)}")

test = r'''#[cfg(unix)]
async fn run_mutate_batch_sandbox_diag_case(
    file_system: &dyn ExecutorFileSystem,
    label: &str,
    batch: FileMutationBatch,
    sandbox: &codex_exec_server::FileSystemSandboxContext,
) -> Result<()> {
    let result = file_system.mutate_batch(batch, Some(sandbox)).await;
    eprintln!("BATCHCASE {label}: {result:?}");
    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_system_mutate_batch_sandbox_diagnostic_matrix() -> Result<()> {
    let context = create_file_system_context(FileSystemImplementation::Local).await?;

    {
        let tmp = TempDir::new()?;
        let sandbox = workspace_write_sandbox(tmp.path().to_path_buf());
        run_mutate_batch_sandbox_diag_case(
            context.file_system.as_ref(),
            "empty-follow-false",
            FileMutationBatch { mutations: vec![], follow_symlinks: false },
            &sandbox,
        ).await?;
    }

    {
        let tmp = TempDir::new()?;
        let path = tmp.path().join("update.txt");
        std::fs::write(&path, b"before")?;
        let sandbox = workspace_write_sandbox(tmp.path().to_path_buf());
        run_mutate_batch_sandbox_diag_case(
            context.file_system.as_ref(),
            "update-follow-false",
            FileMutationBatch {
                mutations: vec![FileMutation::Write {
                    path: PathUri::from_host_native_path(&path)?,
                    expected: FilePreimage::Exact(b"before".to_vec()),
                    contents: b"after".to_vec(),
                }],
                follow_symlinks: false,
            },
            &sandbox,
        ).await?;
    }

    {
        let tmp = TempDir::new()?;
        let path = tmp.path().join("new/a/b/added.txt");
        let sandbox = workspace_write_sandbox(tmp.path().to_path_buf());
        run_mutate_batch_sandbox_diag_case(
            context.file_system.as_ref(),
            "nested-create-follow-false",
            FileMutationBatch {
                mutations: vec![FileMutation::Write {
                    path: PathUri::from_host_native_path(&path)?,
                    expected: FilePreimage::Missing,
                    contents: b"added".to_vec(),
                }],
                follow_symlinks: false,
            },
            &sandbox,
        ).await?;
    }

    {
        let tmp = TempDir::new()?;
        let path = tmp.path().join("delete.txt");
        std::fs::write(&path, b"delete")?;
        let sandbox = workspace_write_sandbox(tmp.path().to_path_buf());
        run_mutate_batch_sandbox_diag_case(
            context.file_system.as_ref(),
            "remove-follow-false",
            FileMutationBatch {
                mutations: vec![FileMutation::Remove {
                    path: PathUri::from_host_native_path(&path)?,
                    expected: b"delete".to_vec(),
                }],
                follow_symlinks: false,
            },
            &sandbox,
        ).await?;
    }

    for follow_symlinks in [false, true] {
        let tmp = TempDir::new()?;
        let update_path = tmp.path().join("update.txt");
        let delete_path = tmp.path().join("delete.txt");
        let nested_path = tmp.path().join("new/a/b/added.txt");
        std::fs::write(&update_path, b"before")?;
        std::fs::write(&delete_path, b"delete")?;
        let sandbox = workspace_write_sandbox(tmp.path().to_path_buf());
        let label = if follow_symlinks { "mixed-follow-true" } else { "mixed-follow-false" };
        run_mutate_batch_sandbox_diag_case(
            context.file_system.as_ref(),
            label,
            FileMutationBatch {
                mutations: vec![
                    FileMutation::Write {
                        path: PathUri::from_host_native_path(&nested_path)?,
                        expected: FilePreimage::Missing,
                        contents: b"added".to_vec(),
                    },
                    FileMutation::Write {
                        path: PathUri::from_host_native_path(&update_path)?,
                        expected: FilePreimage::Exact(b"before".to_vec()),
                        contents: b"after".to_vec(),
                    },
                    FileMutation::Remove {
                        path: PathUri::from_host_native_path(&delete_path)?,
                        expected: b"delete".to_vec(),
                    },
                ],
                follow_symlinks,
            },
            &sandbox,
        ).await?;
    }

    Ok(())
}

'''
path.write_text(text.replace(anchor, test + anchor, 1))
