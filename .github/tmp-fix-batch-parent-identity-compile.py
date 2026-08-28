from pathlib import Path

path = Path("codex-rs/exec-server/src/file_mutation_batch.rs")
text = path.read_text()

text = text.replace(
    "AbsolutePathBuf::from_absolute_path(path)?",
    "AbsolutePathBuf::from_absolute_path(path.clone())?",
    1,
)

metadata_check = '''                let metadata = file_system
                    .get_metadata(
                        &path_uri,
                        GetMetadataOptions { follow_symlinks },
                        /*sandbox*/ None,
                    )
                    .await?;
                if !metadata.is_directory || metadata.is_symlink {
                    return Err(io::Error::other(
                        "created parent is not a stable directory",
                    ));
                }
'''
if metadata_check in text:
    text = text.replace(metadata_check, "", 1)

path.write_text(text)
