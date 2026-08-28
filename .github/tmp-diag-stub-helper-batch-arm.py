from pathlib import Path

path = Path("codex-rs/exec-server/src/fs_helper.rs")
text = path.read_text()
old = '''        FsHelperRequest::MutateBatch(params) => crate::server::mutate_batch(&file_system, params)
            .await
            .map(FsHelperPayload::MutateBatch),
'''
new = '''        FsHelperRequest::MutateBatch(_params) => Err(invalid_request(
            "diagnostic: mutateBatch helper arm disabled".to_string(),
        )),
'''
count = text.count(old)
if count != 1:
    raise RuntimeError(f"expected one mutateBatch helper arm, found {count}")
path.write_text(text.replace(old, new, 1))
