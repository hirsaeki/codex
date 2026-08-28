from pathlib import Path

path = Path("codex-rs/exec-server/src/no_follow/windows.rs")
text = path.read_text()
old = '''        let mut info = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
        let result = unsafe {
            GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info)
        };
'''
new = '''        // SAFETY: BY_HANDLE_FILE_INFORMATION is a plain C output structure and zero is a valid
        // initialization before GetFileInformationByHandle fills every documented field.
        let mut info = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
        // SAFETY: `file` owns a valid handle for the duration of the call and `info` is a valid,
        // writable output buffer.
        let result = unsafe {
            GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info)
        };
'''
if text.count(old) != 1:
    raise RuntimeError(f"Windows identity unsafe block mismatch: {text.count(old)}")
path.write_text(text.replace(old, new, 1))
