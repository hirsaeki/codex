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
