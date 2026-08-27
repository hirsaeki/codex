from pathlib import Path

p = Path("codex-rs/exec-server/src/file_mutation_batch.rs")
s = p.read_text()
s = s.replace(
    """            Ok(postimage) => journal.push(JournalEntry {
                path,
                preimage: match mutation_expected_snapshot(&journal, &path) {
                    Some(snapshot) => snapshot,
                    None => snapshot_from_expected(&planned_expected(&postimage, &path)),
                },
                postimage,
            }),
""",
    """            Ok(entry) => journal.push(entry),
""",
    1,
)
s = s.replace(
    """async fn preflight(
    file_system: &dyn ExecutorFileSystem,
    batch: FileMutationBatch,
) -> io::Result<Vec<PlannedMutation>> {
    if batch.mutations.len() > MAX_FS_MUTATE_BATCH_OPERATIONS {
""",
    """async fn preflight(
    file_system: &dyn ExecutorFileSystem,
    batch: FileMutationBatch,
) -> io::Result<Vec<PlannedMutation>> {
    preflight_with_limits(
        file_system,
        batch,
        MAX_FS_MUTATE_BATCH_OPERATIONS,
        MAX_FS_MUTATE_BATCH_DECODED_BYTES,
    )
    .await
}

async fn preflight_with_limits(
    file_system: &dyn ExecutorFileSystem,
    batch: FileMutationBatch,
    max_operations: usize,
    max_decoded_bytes: usize,
) -> io::Result<Vec<PlannedMutation>> {
    if batch.mutations.len() > max_operations {
""",
    1,
)
s = s.replace(
    "filesystem mutation batch exceeds {MAX_FS_MUTATE_BATCH_OPERATIONS} operations",
    "filesystem mutation batch exceeds {max_operations} operations",
    1,
)
s = s.replace(
    "if decoded_bytes > MAX_FS_MUTATE_BATCH_DECODED_BYTES {",
    "if decoded_bytes > max_decoded_bytes {",
)
s = s.replace(
    "format!(\n            \"filesystem mutation batch exceeds {MAX_FS_MUTATE_BATCH_DECODED_BYTES} decoded bytes\"\n        ),",
    "format!(\"filesystem mutation batch exceeds {max_decoded_bytes} decoded bytes\"),",
    1,
)
s = s.replace(
    "fn batch_too_large_error() -> io::Error {",
    "fn batch_too_large_error(max_decoded_bytes: usize) -> io::Error {",
    1,
)
s = s.replace(".ok_or_else(batch_too_large_error)?;", ".ok_or_else(|| batch_too_large_error(max_decoded_bytes))?;")
s = s.replace("return Err(batch_too_large_error());", "return Err(batch_too_large_error(max_decoded_bytes));")
s = s.replace(
    """    created_directories: &mut Vec<CreatedDirectory>,
) -> Result<Snapshot, MutationFailure> {
""",
    """    created_directories: &mut Vec<CreatedDirectory>,
) -> Result<JournalEntry, MutationFailure> {
""",
    1,
)
s = s.replace(
    """        (Ok(()), Ok(observed)) if observed == intended => Ok(observed),
""",
    """        (Ok(()), Ok(observed)) if observed == intended => Ok(JournalEntry {
            path,
            preimage: planned.preimage,
            postimage: observed,
        }),
""",
    1,
)
# On uncertain post-state, only roll back if the path still equals the intended postimage.
# Never treat an externally observed mismatching value as ours and overwrite it.
s = s.replace(
    """                preimage: planned.preimage,
                postimage: observed,
""",
    """                preimage: planned.preimage.clone(),
                postimage: intended.clone(),
""",
    1,
)
s = s.replace(
    """                preimage: planned.preimage,
                postimage: intended,
""",
    """                preimage: planned.preimage.clone(),
                postimage: intended.clone(),
""",
    1,
)
s = s.replace(
    """                preimage: planned.preimage,
                postimage: observed,
""",
    """                preimage: planned.preimage.clone(),
                postimage: intended.clone(),
""",
    1,
)
s = s.replace(
    """                preimage: planned.preimage,
                postimage: intended,
""",
    """                preimage: planned.preimage.clone(),
                postimage: intended,
""",
    1,
)
marker = "// These helpers intentionally remain tiny; they make ownership moves in the main loop explicit.\n"
if marker in s:
    s = s[: s.index(marker)]
s += '\n#[cfg(test)]\n#[path = "file_mutation_batch_tests.rs"]\nmod tests;\n'
p.write_text(s)
