from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def replace_once(path: Path, old: str, new: str) -> None:
    text = path.read_text(encoding="utf-8")
    if old not in text:
        raise SystemExit(f"expected instrumentation anchor not found in {path}")
    if text.count(old) != 1:
        raise SystemExit(f"instrumentation anchor is not unique in {path}")
    path.write_text(text.replace(old, new, 1), encoding="utf-8")


fs_helper = ROOT / "codex-rs/exec-server/src/fs_helper.rs"
replace_once(
    fs_helper,
    """#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]\n#[serde(tag = \"status\", content = \"payload\", rename_all = \"camelCase\")]\npub(crate) enum FsHelperResponse {\n""",
    """impl FsHelperRequest {\n    pub(crate) fn operation(&self) -> &'static str {\n        match self {\n            Self::DiscoverCapabilityRoots(_) => CAPABILITY_ROOTS_DISCOVER_METHOD,\n            Self::Open(_) => FS_OPEN_METHOD,\n            Self::ReadFile(_) => FS_READ_FILE_METHOD,\n            Self::WriteFile(_) => FS_WRITE_FILE_METHOD,\n            Self::CreateDirectory(_) => FS_CREATE_DIRECTORY_METHOD,\n            Self::GetMetadata(_) => FS_GET_METADATA_METHOD,\n            Self::Canonicalize(_) => FS_CANONICALIZE_METHOD,\n            Self::ReadDirectory(_) => FS_READ_DIRECTORY_METHOD,\n            Self::Walk(_) => FS_WALK_METHOD,\n            Self::Remove(_) => FS_REMOVE_METHOD,\n            Self::Copy(_) => FS_COPY_METHOD,\n        }\n    }\n}\n\n#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]\n#[serde(tag = \"status\", content = \"payload\", rename_all = \"camelCase\")]\npub(crate) enum FsHelperResponse {\n""",
)

fs_sandbox = ROOT / "codex-rs/exec-server/src/fs_sandbox.rs"
replace_once(
    fs_sandbox,
    """use std::collections::HashMap;\n#[cfg(any(windows, test))]\nuse std::time::Duration;\n""",
    """use std::collections::HashMap;\n#[cfg(any(windows, test))]\nuse std::time::Duration;\nuse std::time::Instant;\n""",
)
replace_once(
    fs_sandbox,
    """    pub(crate) async fn run(\n        &self,\n        sandbox: &FileSystemSandboxContext,\n        request: FsHelperRequest,\n    ) -> Result<FsHelperPayload, JSONRPCErrorError> {\n        let command = self.sandbox_command(sandbox)?;\n        let request_json = serde_json::to_vec(&request).map_err(json_error)?;\n        run_command(command, request_json).await\n    }\n""",
    """    pub(crate) async fn run(\n        &self,\n        sandbox: &FileSystemSandboxContext,\n        request: FsHelperRequest,\n    ) -> Result<FsHelperPayload, JSONRPCErrorError> {\n        let operation = request.operation();\n        let started_at = Instant::now();\n        eprintln!(\"CODEX_BASELINE fs_helper phase=start operation={operation}\");\n        let result = async {\n            let command = self.sandbox_command(sandbox)?;\n            let request_json = serde_json::to_vec(&request).map_err(json_error)?;\n            run_command(command, request_json).await\n        }\n        .await;\n        eprintln!(\n            \"CODEX_BASELINE fs_helper phase=complete operation={operation} success={} duration_ms={}\",\n            result.is_ok(),\n            started_at.elapsed().as_millis()\n        );\n        result\n    }\n""",
)

identity = ROOT / "codex-rs/windows-sandbox-rs/src/identity.rs"
replace_once(
    identity,
    """use std::path::Path;\nuse std::path::PathBuf;\n""",
    """use std::path::Path;\nuse std::path::PathBuf;\nuse std::time::Instant;\n""",
)
replace_once(
    identity,
    """    // Always refresh ACLs (non-elevated) for current roots via the setup binary.\n    run_setup_refresh_with_overrides_and_proxy_settings(\n        crate::setup::SandboxSetupRequest {\n            permissions,\n            command_cwd,\n            env_map,\n            codex_home,\n            proxy_enforced,\n        },\n        crate::setup::SetupRootOverrides {\n            read_roots: Some(needed_read),\n            read_roots_include_platform_defaults,\n            write_roots: Some(needed_write),\n            deny_read_paths: Some(deny_read_paths_override.to_vec()),\n            deny_write_paths: Some(deny_write_paths_override.to_vec()),\n        },\n        &desired_offline_proxy_settings,\n    )?;\n""",
    """    // Always refresh ACLs (non-elevated) for current roots via the setup binary.\n    let refresh_started_at = Instant::now();\n    crate::logging::log_note(\"CODEX_BASELINE setup_refresh phase=start\", Some(&sandbox_dir));\n    let refresh_result = run_setup_refresh_with_overrides_and_proxy_settings(\n        crate::setup::SandboxSetupRequest {\n            permissions,\n            command_cwd,\n            env_map,\n            codex_home,\n            proxy_enforced,\n        },\n        crate::setup::SetupRootOverrides {\n            read_roots: Some(needed_read),\n            read_roots_include_platform_defaults,\n            write_roots: Some(needed_write),\n            deny_read_paths: Some(deny_read_paths_override.to_vec()),\n            deny_write_paths: Some(deny_write_paths_override.to_vec()),\n        },\n        &desired_offline_proxy_settings,\n    );\n    crate::logging::log_note(\n        &format!(\n            \"CODEX_BASELINE setup_refresh phase=complete success={} duration_ms={}\",\n            refresh_result.is_ok(),\n            refresh_started_at.elapsed().as_millis()\n        ),\n        Some(&sandbox_dir),\n    );\n    refresh_result?;\n""",
)

cargo = ROOT / "codex-rs/apply-patch/Cargo.toml"
replace_once(
    cargo,
    """codex-exec-server = { workspace = true }\n""",
    """codex-exec-server = { workspace = true }\ncodex-protocol = { workspace = true }\n""",
)

harness = ROOT / "codex-rs/apply-patch/src/bin/apply_patch_sandbox_baseline.rs"
harness.parent.mkdir(parents=True, exist_ok=True)
harness.write_text(
    r'''use anyhow::Context;
use codex_apply_patch::apply_patch;
use codex_exec_server::ExecServerRuntimePaths;
use codex_exec_server::FileSystemSandboxContext;
use codex_exec_server::LocalFileSystem;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::protocol::SandboxPolicy;
use codex_utils_path_uri::PathUri;
use std::path::PathBuf;
use std::time::Instant;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let workspace = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .context("usage: apply_patch_sandbox_baseline.exe <workspace>")?;
    std::fs::create_dir_all(&workspace)?;
    let cwd = PathUri::from_host_native_path(&workspace)?;
    let codex_exe = std::env::var_os("CODEX_BASELINE_CODEX_EXE")
        .map(PathBuf::from)
        .context("CODEX_BASELINE_CODEX_EXE is required")?;
    let runtime_paths = ExecServerRuntimePaths::new(codex_exe, None)?;
    let fs = LocalFileSystem::with_runtime_paths(runtime_paths);
    let mut sandbox = FileSystemSandboxContext::from_legacy_sandbox_policy(
        SandboxPolicy::new_workspace_write_policy(),
        cwd.clone(),
    )?;
    sandbox.windows_sandbox_level = WindowsSandboxLevel::Elevated;

    let cases = [
        (
            "create",
            "*** Begin Patch\n*** Add File: baseline.txt\n+one\n*** End Patch",
        ),
        (
            "update",
            "*** Begin Patch\n*** Update File: baseline.txt\n@@\n-one\n+two\n*** End Patch",
        ),
        (
            "delete",
            "*** Begin Patch\n*** Delete File: baseline.txt\n*** End Patch",
        ),
    ];

    for (name, patch) in cases {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        eprintln!("CODEX_BASELINE apply_patch phase=start operation={name}");
        let started_at = Instant::now();
        let result = apply_patch(
            patch,
            &cwd,
            &mut stdout,
            &mut stderr,
            &fs,
            Some(&sandbox),
        )
        .await;
        eprintln!(
            "CODEX_BASELINE apply_patch phase=complete operation={name} success={} duration_ms={}",
            result.is_ok(),
            started_at.elapsed().as_millis()
        );
        if let Err(err) = result {
            anyhow::bail!(
                "{name} failed: {err}; stdout={}; stderr={}",
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            );
        }
    }
    Ok(())
}
''',
    encoding="utf-8",
)

print("apply_patch baseline instrumentation and harness applied")
