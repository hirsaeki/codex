from pathlib import Path

path = Path("codex-rs/exec-server/tests/file_system_windows.rs")
text = path.read_text()
anchor = "use codex_exec_server::FileSystemSandboxContext;\n"
imports = anchor + "use codex_exec_server::FileMutation;\nuse codex_exec_server::FileMutationBatch;\nuse codex_exec_server::FileMutationBatchOutcome;\nuse codex_exec_server::FilePreimage;\n"
if text.count(anchor) != 1:
    raise RuntimeError("windows import anchor mismatch")
text = text.replace(anchor, imports, 1)

insert_before = '''#[test_case(FileSystemImplementation::Local ; "local")]
#[test_case(FileSystemImplementation::Remote ; "remote")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_system_operations_can_reject_junctions_in_any_path_component(
'''
if text.count(insert_before) != 1:
    raise RuntimeError("windows batch test anchor mismatch")

test = r'''#[test_case(FileSystemImplementation::Local ; "local")]
#[test_case(FileSystemImplementation::Remote ; "remote")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_system_mutate_batch_rejects_junction_ancestors(
    implementation: FileSystemImplementation,
) -> Result<()> {
    let context = create_file_system_context(implementation).await?;

    for sandboxed in [false, true] {
        let tmp = tempfile::TempDir::new()?;
        let real = tmp.path().join("real");
        std::fs::create_dir(&real)?;
        let junction = tmp.path().join("junction");
        create_directory_junction(&real, &junction)?;
        let escaped = junction.join("nested/file.txt");
        let sandbox = sandboxed.then(|| workspace_write_sandbox(tmp.path().to_path_buf()));
        let batch = FileMutationBatch {
            mutations: vec![FileMutation::Write {
                path: PathUri::from_host_native_path(&escaped)?,
                expected: FilePreimage::Missing,
                contents: b"escape".to_vec(),
            }],
            follow_symlinks: false,
        };

        let result = context.file_system.mutate_batch(batch, sandbox.as_ref()).await;
        if sandboxed && is_unsupported_restricted_token_host(&result) {
            continue;
        }
        let outcome = result?;
        assert!(
            matches!(outcome, FileMutationBatchOutcome::Rejected { .. }),
            "unexpected batch outcome: {outcome:?}"
        );
        assert!(!real.join("nested").exists());
    }

    Ok(())
}

'''
text = text.replace(insert_before, test + insert_before, 1)
path.write_text(text)
