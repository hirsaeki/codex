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

#[ctor]
static CODEX_DISPATCH: Option<TestBinaryDispatchGuard> = {
    configure_test_binary_dispatch(
        "codex-windows-apply-patch-fs-helper-reuse",
        |_exe_name, argv1| {
            if matches!(
                argv1,
                Some(CODEX_FS_HELPER_ARG1) | Some(CODEX_WINDOWS_SANDBOX_ARG1)
            ) {
                TestBinaryDispatchMode::DispatchArg0Only
            } else {
                TestBinaryDispatchMode::InstallAliases
            }
        },
    )
};

#[test]
fn one_apply_patch_reuses_one_filesystem_helper() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build test runtime")?;
    runtime.block_on(run_test())
}

async fn run_test() -> Result<()> {
    let codex_home = codex_utils_home_dir::find_codex_home().context("resolve CODEX_HOME")?;
    let workspace = tempfile::tempdir().context("create test workspace")?;
    let workspace = AbsolutePathBuf::from_absolute_path(workspace.path())?;
    let cwd = PathUri::from_abs_path(&workspace);
    let target_path = workspace.join("reuse.txt");

    let runtime_paths = ExecServerRuntimePaths::new(
        std::env::current_exe().context("resolve test executable")?,
        /*codex_linux_sandbox_exe*/ None,
    )?;
    let fs = LocalFileSystem::with_runtime_paths(runtime_paths);
    let sandbox = FileSystemSandboxContext {
        permissions: PermissionProfile::workspace_write().into(),
        cwd: Some(cwd.clone()),
        workspace_roots: vec![cwd.clone()],
        user_home_dir: None,
        temporary_directories: None,
        windows_sandbox_level: WindowsSandboxLevel::Elevated,
        windows_sandbox_private_desktop: false,
        windows_sandbox_proxy_settings_mode: None,
        use_legacy_landlock: false,
    };

    // Provision once so the assertion below observes the helper lifecycle for
    // the logical apply_patch operation rather than first-run sandbox setup.
    fs.get_metadata(&cwd, GetMetadataOptions::default(), Some(&sandbox))
        .await
        .context("warm up Windows sandbox")?;

    let log_path = codex_windows_sandbox::current_log_file_path_for_codex_home(&codex_home);
    let log_line_count = std::fs::read_to_string(&log_path)
        .with_context(|| format!("read warm-up sandbox log at {}", log_path.display()))?
        .lines()
        .count();

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let (result, helper_cleanup) = codex_exec_server::with_apply_patch_fs_helper_reuse(
        codex_apply_patch::apply_patch_with_options(
            "*** Begin Patch\n*** Add File: reuse.txt\n+one\n*** End Patch\n",
            ApplyPatchOptions::default(),
            &cwd,
            &mut stdout,
            &mut stderr,
            &fs,
            Some(&sandbox),
        ),
    )
    .await;
    result.map_err(|error| anyhow::anyhow!("apply_patch failed: {error}"))?;
    helper_cleanup.context("cleanup apply_patch filesystem helper")?;

    anyhow::ensure!(
        std::fs::read_to_string(&target_path)? == "one\n",
        "created file contents did not match"
    );

    let log = std::fs::read_to_string(&log_path)
        .with_context(|| format!("read sandbox log at {}", log_path.display()))?;
    let helper_starts = log
        .lines()
        .skip(log_line_count)
        .filter(|line| line.contains("START:") && line.contains(CODEX_FS_HELPER_ARG1))
        .count();
    anyhow::ensure!(
        helper_starts == 1,
        "expected exactly one filesystem helper for one apply_patch, got {helper_starts}"
    );
    let operation_log = log.lines().skip(log_line_count).collect::<Vec<_>>();
    let setup_refreshes = operation_log
        .iter()
        .filter(|line| line.contains("setup refresh: running in-process"))
        .count();
    let refresh_child_spawns = operation_log
        .iter()
        .filter(|line| line.contains("setup refresh: spawning "))
        .count();
    anyhow::ensure!(
        setup_refreshes == 1,
        "expected exactly one in-process sandbox setup refresh, got {setup_refreshes}"
    );
    anyhow::ensure!(
        refresh_child_spawns == 0,
        "expected no refresh-only setup child process, got {refresh_child_spawns}"
    );

    Ok(())
}
