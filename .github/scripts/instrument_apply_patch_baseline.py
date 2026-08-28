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
    """    pub(crate) async fn run(\n        &self,\n        sandbox: &FileSystemSandboxContext,\n        request: FsHelperRequest,\n    ) -> Result<FsHelperPayload, JSONRPCErrorError> {\n        let operation = request.operation();\n        let started_at = Instant::now();\n        tracing::info!(\n            target: \"codex_apply_patch_baseline\",\n            event = \"fs_helper\",\n            phase = \"start\",\n            operation,\n            \"sandbox filesystem helper started\"\n        );\n        let result = async {\n            let command = self.sandbox_command(sandbox)?;\n            let request_json = serde_json::to_vec(&request).map_err(json_error)?;\n            run_command(command, request_json).await\n        }\n        .await;\n        tracing::info!(\n            target: \"codex_apply_patch_baseline\",\n            event = \"fs_helper\",\n            phase = \"complete\",\n            operation,\n            success = result.is_ok(),\n            duration_ms = started_at.elapsed().as_millis(),\n            \"sandbox filesystem helper completed\"\n        );\n        result\n    }\n""",
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
    """    // Always refresh ACLs (non-elevated) for current roots via the setup binary.\n    let refresh_started_at = Instant::now();\n    crate::logging::log_note(\n        \"setup refresh: started\",\n        Some(&sandbox_dir),\n    );\n    let refresh_result = run_setup_refresh_with_overrides_and_proxy_settings(\n        crate::setup::SandboxSetupRequest {\n            permissions,\n            command_cwd,\n            env_map,\n            codex_home,\n            proxy_enforced,\n        },\n        crate::setup::SetupRootOverrides {\n            read_roots: Some(needed_read),\n            read_roots_include_platform_defaults,\n            write_roots: Some(needed_write),\n            deny_read_paths: Some(deny_read_paths_override.to_vec()),\n            deny_write_paths: Some(deny_write_paths_override.to_vec()),\n        },\n        &desired_offline_proxy_settings,\n    );\n    crate::logging::log_note(\n        &format!(\n            \"setup refresh: completed success={} duration_ms={}\",\n            refresh_result.is_ok(),\n            refresh_started_at.elapsed().as_millis()\n        ),\n        Some(&sandbox_dir),\n    );\n    refresh_result?;\n""",
)

print("apply_patch baseline instrumentation applied")
