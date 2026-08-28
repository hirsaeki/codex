from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    if text.count(old) != 1:
        raise RuntimeError(f"{path}: expected one match for {old[:80]!r}, got {text.count(old)}")
    p.write_text(text.replace(old, new, 1))


# The execution-time prepared path uses derive_new_contents_from_contents directly.
path = "codex-rs/apply-patch/src/file_update.rs"
p = Path(path)
text = p.read_text()
start = text.index("pub(crate) fn file_update_from_contents(")
end = text.index("/// Compute a list of replacements needed", start)
text = text[:start] + text[end:]
p.write_text(text)

# Add batch-specific seam tests without changing production exports.
path = "codex-rs/apply-patch/src/lib.rs"
p = Path(path)
text = p.read_text()
marker = '#[cfg(test)]\nmod tests {'
if marker not in text:
    raise RuntimeError("apply-patch test module marker missing")
text = text.replace(
    marker,
    '#[cfg(test)]\n#[path = "batch_tests.rs"]\nmod batch_tests;\n\n#[cfg(test)]\nmod tests {',
    1,
)
p.write_text(text)

# Remove imports made obsolete by From conversions in the current protocol shape.
path = "codex-rs/exec-server/src/server/file_system_handler.rs"
p = Path(path)
text = p.read_text()
text = text.replace("use crate::FileMutationBatchOutcome;\n", "", 1)
text = text.replace("use crate::protocol::FsMutationBatchOutcome;\n", "", 1)
p.write_text(text)

# Treat JSON-RPC error codes as values in guards, avoiding accidental pattern bindings.
path = "codex-rs/exec-server/src/remote_file_system.rs"
p = Path(path)
text = p.read_text()
if "const METHOD_NOT_FOUND_ERROR_CODE" not in text:
    anchor = "const INVALID_REQUEST_ERROR_CODE: i64 = -32600;\n"
    if anchor not in text:
        raise RuntimeError("remote error-code anchor missing")
    text = text.replace(
        anchor,
        anchor + "const METHOD_NOT_FOUND_ERROR_CODE: i64 = -32601;\n",
        1,
    )
text = text.replace(
    '''            Err(ExecServerError::Server {
                code: INVALID_REQUEST_ERROR_CODE,
                message,
            }) => FileMutationBatchOutcome::Rejected { error: message },
''',
    '''            Err(ExecServerError::Server { code, message })
                if code == INVALID_REQUEST_ERROR_CODE =>
            {
                FileMutationBatchOutcome::Rejected { error: message }
            }
''',
    1,
)
text = text.replace(
    '''            Err(ExecServerError::Server {
                code: METHOD_NOT_FOUND_ERROR_CODE,
                message,
            }) => {
''',
    '''            Err(ExecServerError::Server { code, message })
                if code == METHOD_NOT_FOUND_ERROR_CODE =>
            {
''',
    1,
)
p.write_text(text)

# The mismatch branch deliberately ignores the observed external value and rolls back only
# if the path still equals the intended postimage.
path = "codex-rs/exec-server/src/file_mutation_batch.rs"
p = Path(path)
text = p.read_text().replace(
    "        (Ok(()), Ok(observed)) => Err(MutationFailure {\n",
    "        (Ok(()), Ok(_observed)) => Err(MutationFailure {\n",
    1,
)
p.write_text(text)

# Keep the focused batch test module warning-free and cover rollback of created parent trees.
path = "codex-rs/exec-server/src/file_mutation_batch_tests.rs"
p = Path(path)
if p.exists():
    text = p.read_text().replace("use crate::FileSystemResult;\n", "", 1)
    anchor = '''#[tokio::test]
async fn rollback_failure_is_indeterminate() -> io::Result<()> {
'''
    test = '''#[tokio::test]
async fn nested_parent_creation_is_rolled_back_after_later_failure() -> io::Result<()> {
    let temp = TempDir::new()?;
    let nested = temp.path().join("new/a/b/file.txt");
    let second = temp.path().join("second.txt");
    std::fs::write(&second, b"before")?;
    let fs = FailingFs::new(Some(2), false);

    let outcome = mutate_batch_on(
        &fs,
        batch(
            vec![
                write(&nested, FilePreimage::Missing, b"added"),
                write(&second, FilePreimage::Exact(b"before".to_vec()), b"after"),
            ],
            false,
        ),
    )
    .await;

    assert!(matches!(outcome, FileMutationBatchOutcome::RolledBack { .. }));
    assert!(!nested.exists());
    assert!(!temp.path().join("new").exists());
    assert_eq!(std::fs::read(second)?, b"before");
    Ok(())
}

'''
    if text.count(anchor) != 1:
        raise RuntimeError("batch nested rollback test anchor mismatch")
    text = text.replace(anchor, test + anchor, 1)
    p.write_text(text)
