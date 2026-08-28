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
text = text.replace(old, new, 1)

marker = '''    #[test]
    fn helper_protocol_uses_path_uris() -> serde_json::Result<()> {
'''
diag = '''    #[test]
    fn diagnostic_helper_layout_sizes() {
        use std::mem::size_of;
        use std::mem::size_of_val;

        let path = PathUri::from_host_native_path(
            std::env::current_dir().expect("cwd").join("diag-file"),
        )
        .expect("path URI");
        let future = run_direct_request(FsHelperRequest::WriteFile(FsWriteFileParams {
            path,
            data_base64: String::new(),
            follow_symlinks: None,
            sandbox: None,
        }));
        eprintln!(
            "HELPER_LAYOUT request={} payload={} direct_future={}",
            size_of::<FsHelperRequest>(),
            size_of::<FsHelperPayload>(),
            size_of_val(&future),
        );
    }

'''
if text.count(marker) != 1:
    raise RuntimeError(f"helper test marker mismatch: {text.count(marker)}")
text = text.replace(marker, diag + marker, 1)
path.write_text(text)
