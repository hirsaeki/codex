#[cfg(target_os = "windows")]
mod win;

#[cfg(target_os = "windows")]
fn main() -> anyhow::Result<()> {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use std::path::PathBuf;
    use std::time::Instant;

    let refresh_codex_home = std::env::args().nth(1).and_then(|payload_b64| {
        BASE64
            .decode(payload_b64)
            .ok()
            .and_then(|payload| serde_json::from_slice::<serde_json::Value>(&payload).ok())
            .and_then(|payload| {
                let is_refresh_only_full = payload
                    .get("refresh_only")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                    && payload
                        .get("mode")
                        .and_then(serde_json::Value::as_str)
                        .is_none_or(|mode| mode == "full");
                is_refresh_only_full
                    .then(|| {
                        payload
                            .get("codex_home")
                            .and_then(serde_json::Value::as_str)
                            .map(PathBuf::from)
                    })
                    .flatten()
            })
    });

    let started_at = Instant::now();
    let result = win::main();
    let elapsed_ms = started_at.elapsed().as_secs_f64() * 1000.0;

    if let Some(codex_home) = refresh_codex_home {
        let sandbox_dir = codex_windows_sandbox::sandbox_dir(&codex_home);
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
