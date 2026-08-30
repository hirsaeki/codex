#![cfg(target_os = "windows")]

use anyhow::Context;
use anyhow::Result;
use codex_apply_patch::ApplyPatchOptions;
use codex_exec_server::CODEX_FS_HELPER_ARG1;
use codex_exec_server::ExecServerRuntimePaths;
use codex_exec_server::ExecutorFileSystem;
use codex_exec_server::FileSystemSandboxContext;
use codex_exec_server::GetMetadataOptions;
use codex_exec_server::LocalFileSystem;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::PermissionProfile;
use codex_test_binary_support::TestBinaryDispatchGuard;
use codex_test_binary_support::TestBinaryDispatchMode;
use codex_test_binary_support::configure_test_binary_dispatch;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use codex_windows_sandbox::CODEX_WINDOWS_SANDBOX_ARG1;
use ctor::ctor;
use serde::Serialize;
use std::path::Path;
use std::time::Instant;

#[ctor]
static CODEX_DISPATCH: Option<TestBinaryDispatchGuard> = {
    configure_test_binary_dispatch("codex-windows-apply-patch-baseline", |_exe_name, argv1| {
        if matches!(
            argv1,
            Some(CODEX_FS_HELPER_ARG1) | Some(CODEX_WINDOWS_SANDBOX_ARG1)
        ) {
            TestBinaryDispatchMode::DispatchArg0Only
        } else {
            TestBinaryDispatchMode::InstallAliases
        }
    })
};

#[derive(Debug, Default)]
struct SandboxLogStats {
    helper_starts: usize,
    setup_refreshes: usize,
    setup_refresh_total_ms: f64,
}

#[derive(Serialize)]
struct BaselineResult<'a> {
    operation: &'a str,
    elapsed_ms: f64,
    fs_helper_starts: usize,
    setup_refreshes: usize,
    setup_refresh_total_ms: f64,
}

#[test]
fn native_windows_apply_patch_baseline() -> Result<()> {
    tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_target(false)
        .with_max_level(tracing::Level::DEBUG)
        .try_init()
        .map_err(|error| anyhow::anyhow!("install baseline tracing subscriber: {error}"))?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build baseline runtime")?;
    runtime.block_on(run_baseline())
}

async fn run_baseline() -> Result<()> {
    let codex_home = codex_utils_home_dir::find_codex_home().context("resolve CODEX_HOME")?;
    let workspace = tempfile::tempdir().context("create baseline workspace")?;
    let workspace = AbsolutePathBuf::from_absolute_path(workspace.path())?;
    let cwd = PathUri::from_abs_path(&workspace);
    let target_path = workspace.join("baseline.txt");

    let runtime_paths = ExecServerRuntimePaths::new(
        std::env::current_exe().context("resolve baseline test executable")?,
        /*codex_linux_sandbox_exe*/ None,
    )?;
    let fs = LocalFileSystem::with_runtime_paths(runtime_paths);
    let sandbox = FileSystemSandboxContext {
        permissions: PermissionProfile::workspace_write().into(),
        cwd: Some(cwd.clone()),
        workspace_roots: vec![cwd.clone()],
        temporary_directories: None,
        windows_sandbox_level: WindowsSandboxLevel::Elevated,
        windows_sandbox_private_desktop: false,
        windows_sandbox_proxy_settings_mode: None,
        use_legacy_landlock: false,
    };

    // Provision the sandbox once before measuring so create/update/delete do not include
    // first-run account/setup creation. The measured elevated path still performs its normal
    // per-helper setup refresh.
    fs.get_metadata(&cwd, GetMetadataOptions::default(), Some(&sandbox))
        .await
        .context("warm up Windows sandbox")?;

    let log_path = codex_windows_sandbox::current_log_file_path_for_codex_home(&codex_home);
    let mut log_line_count = std::fs::read_to_string(&log_path)
        .with_context(|| format!("read warm-up sandbox log at {}", log_path.display()))?
        .lines()
        .count();

    log_line_count = run_patch(
        "create",
        "*** Begin Patch\n*** Add File: baseline.txt\n+one\n*** End Patch\n",
        &cwd,
        &fs,
        &sandbox,
        &log_path,
        log_line_count,
    )
    .await?;
    anyhow::ensure!(
        std::fs::read_to_string(&target_path)? == "one\n",
        "create fixture contents did not match"
    );

    log_line_count = run_patch(
        "update",
        "*** Begin Patch\n*** Update File: baseline.txt\n@@\n-one\n+two\n*** End Patch\n",
        &cwd,
        &fs,
        &sandbox,
        &log_path,
        log_line_count,
    )
    .await?;
    anyhow::ensure!(
        std::fs::read_to_string(&target_path)? == "two\n",
        "update fixture contents did not match"
    );

    let _ = run_patch(
        "delete",
        "*** Begin Patch\n*** Delete File: baseline.txt\n*** End Patch\n",
        &cwd,
        &fs,
        &sandbox,
        &log_path,
        log_line_count,
    )
    .await?;
    anyhow::ensure!(
        !target_path.exists(),
        "delete fixture still exists after apply_patch"
    );

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_patch(
    operation: &'static str,
    patch: &str,
    cwd: &PathUri,
    fs: &LocalFileSystem,
    sandbox: &FileSystemSandboxContext,
    log_path: &Path,
    log_line_count: usize,
) -> Result<usize> {
    println!("APPLY_PATCH_BASELINE_BEGIN {operation}");
    let started_at = Instant::now();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let result = codex_apply_patch::apply_patch_with_options(
        patch,
        ApplyPatchOptions::default(),
        cwd,
        &mut stdout,
        &mut stderr,
        fs,
        Some(sandbox),
    )
    .await;
    let elapsed_ms = started_at.elapsed().as_secs_f64() * 1000.0;
    if let Err(error) = result {
        anyhow::bail!(
            "{operation} apply_patch failed: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
    }

    let log = std::fs::read_to_string(log_path)
        .with_context(|| format!("read sandbox log at {}", log_path.display()))?;
    let lines = log.lines().collect::<Vec<_>>();
    let stats = summarize_sandbox_log(&lines[log_line_count.min(lines.len())..]);
    let result = BaselineResult {
        operation,
        elapsed_ms,
        fs_helper_starts: stats.helper_starts,
        setup_refreshes: stats.setup_refreshes,
        setup_refresh_total_ms: stats.setup_refresh_total_ms,
    };
    println!(
        "APPLY_PATCH_BASELINE_RESULT {}",
        serde_json::to_string(&result).context("serialize baseline result")?
    );
    println!("APPLY_PATCH_BASELINE_END {operation}");
    Ok(lines.len())
}

fn summarize_sandbox_log(lines: &[&str]) -> SandboxLogStats {
    let mut stats = SandboxLogStats::default();
    for line in lines {
        if line.contains("START:") && line.contains(CODEX_FS_HELPER_ARG1) {
            stats.helper_starts += 1;
        }
        let Some((_, completion)) = line.split_once("setup refresh: completed success=") else {
            continue;
        };
        stats.setup_refreshes += 1;
        if let Some((_, elapsed)) = completion.split_once(" elapsed_ms=")
            && let Some(value) = elapsed.split_whitespace().next()
            && let Ok(value) = value.parse::<f64>()
        {
            stats.setup_refresh_total_ms += value;
        }
    }
    stats
}
