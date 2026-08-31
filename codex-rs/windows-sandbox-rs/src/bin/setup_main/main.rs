#[cfg(target_os = "windows")]
mod win;

#[cfg(target_os = "windows")]
fn main() -> anyhow::Result<()> {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use std::path::Path;
    use std::time::Instant;

    let started_at = Instant::now();
    let result = win::main();
    let elapsed_ms = started_at.elapsed().as_secs_f64() * 1000.0;

    let is_refresh_only_full = std::env::args().nth(1).is_some_and(|payload_b64| {
        BASE64
            .decode(payload_b64)
            .ok()
            .and_then(|payload| serde_json::from_slice::<serde_json::Value>(&payload).ok())
            .is_some_and(|payload| {
                payload.get("refresh_only").and_then(serde_json::Value::as_bool) == Some(true)
                    && payload
                        .get("mode")
                        .and_then(serde_json::Value::as_str)
                        .is_none_or(|mode| mode == "full")
            })
    });
    if is_refresh_only_full
        && let Ok(codex_home) = std::env::var("CODEX_HOME")
    {
        let sandbox_dir = codex_windows_sandbox::sandbox_dir(Path::new(&codex_home));
        codex_windows_sandbox::log_note(
            &format!(
                "setup refresh child main: completed success={} elapsed_ms={elapsed_ms:.3}",
                result.is_ok()
            ),
            Some(sandbox_dir.as_path()),
        );
    }

    result
}

#[cfg(not(target_os = "windows"))]
fn main() {
    panic!("codex-windows-sandbox-setup is Windows-only");
}
