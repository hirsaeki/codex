from pathlib import Path

path = Path("codex-rs/exec-server/src/local_file_system.rs")
text = path.read_text()
old = '''        // Once spawned, dropping this future does not cancel the mutation or its rollback.
        // A join failure therefore cannot prove that the filesystem is unchanged.
        match tokio::spawn(async move { crate::file_mutation_batch::mutate_batch(batch).await }).await {
            Ok(outcome) => Ok(outcome),
            Err(error) => Ok(FileMutationBatchOutcome::Indeterminate {
                error: format!("filesystem task failed: {error}"),
                possibly_mutated_paths,
            }),
        }
'''
new = '''        let _ = possibly_mutated_paths;
        Ok(crate::file_mutation_batch::mutate_batch(batch).await)
'''
if text.count(old) != 1:
    raise RuntimeError(f"expected one direct batch spawn block, found {text.count(old)}")
path.write_text(text.replace(old, new, 1))
