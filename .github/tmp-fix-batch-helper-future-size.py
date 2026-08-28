from pathlib import Path

path = Path("codex-rs/exec-server/src/fs_helper.rs")
text = path.read_text()
old = '''        FsHelperRequest::MutateBatch(params) => crate::server::mutate_batch(&file_system, params)
            .await
            .map(FsHelperPayload::MutateBatch),
'''
new = '''        // Keep the transaction future off the one-shot helper's top-level async state machine.
        // This branch is substantially larger than the primitive filesystem requests; boxing it
        // prevents every helper invocation from inheriting that stack footprint.
        FsHelperRequest::MutateBatch(params) => Box::pin(crate::server::mutate_batch(
            &file_system,
            params,
        ))
        .await
        .map(FsHelperPayload::MutateBatch),
'''
count = text.count(old)
if count != 1:
    raise RuntimeError(f"expected one mutateBatch helper arm, found {count}")
path.write_text(text.replace(old, new, 1))
