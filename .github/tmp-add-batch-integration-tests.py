from pathlib import Path

path = Path("codex-rs/exec-server/tests/file_system/shared.rs")
text = path.read_text()
anchor = "use codex_exec_server::FileMetadata;\n"
imports = anchor + "use codex_exec_server::FileMutation;\nuse codex_exec_server::FileMutationBatch;\nuse codex_exec_server::FileMutationBatchOutcome;\nuse codex_exec_server::FilePreimage;\n"
if text.count(anchor) != 1:
    raise RuntimeError("shared import anchor mismatch")
text = text.replace(anchor, imports, 1)

insert_before = "#[test_case(FileSystemImplementation::Local ; \"local\")]\n#[test_case(FileSystemImplementation::Remote ; \"remote\")]\n#[tokio::test(flavor = \"multi_thread\", worker_threads = 2)]\nasync fn file_system_get_metadata_reports_files_and_directories(\n"
if text.count(insert_before) != 1:
    raise RuntimeError("shared test insertion anchor mismatch")

test = r'''#[test_case(FileSystemImplementation::Local ; "local")]
#[test_case(FileSystemImplementation::Remote ; "remote")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_system_mutate_batch_round_trips_unsandboxed_and_sandboxed(
    implementation: FileSystemImplementation,
) -> Result<()> {
    let context = create_file_system_context(implementation).await?;

    for sandboxed in [false, true] {
        let tmp = TempDir::new()?;
        let update_path = tmp.path().join("update.txt");
        let delete_path = tmp.path().join("delete.txt");
        let nested_path = tmp.path().join("new/a/b/added.txt");
        std::fs::write(&update_path, b"before")?;
        std::fs::write(&delete_path, b"delete")?;
        let sandbox = sandboxed.then(|| workspace_write_sandbox(tmp.path().to_path_buf()));
        let uri = |path: &Path| PathUri::from_host_native_path(path);
        let batch = FileMutationBatch {
            mutations: vec![
                FileMutation::Write {
                    path: uri(&nested_path)?,
                    expected: FilePreimage::Missing,
                    contents: b"added".to_vec(),
                },
                FileMutation::Write {
                    path: uri(&update_path)?,
                    expected: FilePreimage::Exact(b"before".to_vec()),
                    contents: b"after".to_vec(),
                },
                FileMutation::Remove {
                    path: uri(&delete_path)?,
                    expected: b"delete".to_vec(),
                },
            ],
            follow_symlinks: false,
        };

        let result = context
            .file_system
            .mutate_batch(batch, sandbox.as_ref())
            .await;
        #[cfg(windows)]
        if sandboxed && is_unsupported_restricted_token_host(&result) {
            continue;
        }
        let outcome = result.with_context(|| {
            format!("mode={implementation}, sandboxed={sandboxed}")
        })?;
        assert_eq!(outcome, FileMutationBatchOutcome::Committed);
        assert_eq!(std::fs::read(nested_path)?, b"added");
        assert_eq!(std::fs::read(update_path)?, b"after");
        assert!(!delete_path.exists());
    }

    Ok(())
}

'''
text = text.replace(insert_before, test + insert_before, 1)
path.write_text(text)
