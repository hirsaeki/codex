from pathlib import Path

path = Path("codex-rs/exec-server/src/file_mutation_batch_tests.rs")
text = path.read_text()
needle = "use crate::ExecutorFileSystemFuture;\n"
replacement = needle + "use crate::FileMetadata;\n"
if text.count(needle) != 1:
    raise RuntimeError(f"expected one ExecutorFileSystemFuture import, got {text.count(needle)}")
if "use crate::FileMetadata;\n" not in text:
    text = text.replace(needle, replacement, 1)
path.write_text(text)
