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
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tracing::Instrument;

static MEASUREMENT_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

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
    setup_refresh_child_execution_ms: Option<f64>,
    sandbox_prepare_total_ms: Option<f64>,
    sandbox_runner_launch_total_ms: Option<f64>,
}

#[derive(Serialize)]
struct BaselineResult<'a> {
    operation: &'a str,
    elapsed_ms: f64,
    fs_helper_starts: usize,
    setup_refreshes: usize,
    first_fs_request_ms: f64,
    setup_refresh_total_ms: f64,
    setup_refresh_child_execution_ms: f64,
    setup_refresh_orchestration_estimated_ms: f64,
    sandbox_prepare_total_ms: f64,
    sandbox_prepare_excluding_refresh_ms: f64,
    sandbox_runner_launch_total_ms: f64,
    first_fs_unattributed_residual_ms: f64,
}

struct TeeWriter {
    capture: Arc<Mutex<Vec<u8>>>,
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        std::io::stderr().write_all(buf)?;
        self.capture
            .lock()
            .expect("trace capture lock")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()
    }
}

#[test]
fn native_windows_apply_patch_baseline() -> Result<()> {
    let trace_capture = Arc::new(Mutex::new(Vec::new()));
    let writer_capture = Arc::clone(&trace_capture);
    tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_target(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(move || TeeWriter {
            capture: Arc::clone(&writer_capture),
        })
        .try_init()
        .map_err(|error| anyhow::anyhow!("install baseline tracing subscriber: {error}"))?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build baseline runtime")?;
    runtime.block_on(run_baseline(trace_capture))
}

async fn run_baseline(trace_capture: Arc<Mutex<Vec<u8>>>) -> Result<()> {
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
        user_home_dir: None,
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
        &trace_capture,
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
        &trace_capture,
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
        &trace_capture,
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
    trace_capture: &Arc<Mutex<Vec<u8>>>,
) -> Result<usize> {
    println!("APPLY_PATCH_BASELINE_BEGIN {operation}");
    let measurement_id = format!(
        "{operation}-{}",
        MEASUREMENT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let trace_offset = trace_capture.lock().expect("trace capture lock").len();
    let started_at = Instant::now();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let span = tracing::info_span!("apply_patch_p2m", measurement_id = %measurement_id);
    let result = codex_exec_server::with_apply_patch_fs_helper_reuse(
        codex_apply_patch::apply_patch_with_options(
            patch,
            ApplyPatchOptions::default(),
            cwd,
            &mut stdout,
            &mut stderr,
            fs,
            Some(sandbox),
        ),
    )
    .instrument(span)
    .await;
    let elapsed_ms = started_at.elapsed().as_secs_f64() * 1000.0;
    if let Err(error) = result {
        anyhow::bail!(
            "{operation} apply_patch failed: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
    }

    let trace = trace_capture.lock().expect("trace capture lock");
    let trace_slice = &trace[trace_offset.min(trace.len())..];
    let first_fs_request_ms = first_fs_request_elapsed_ms(trace_slice, &measurement_id)
        .with_context(|| format!("find first filesystem request timing for {operation}"))?;
    drop(trace);

    let log = std::fs::read_to_string(log_path)
        .with_context(|| format!("read sandbox log at {}", log_path.display()))?;
    let lines = log.lines().collect::<Vec<_>>();
    let stats = summarize_sandbox_log(&lines[log_line_count.min(lines.len())..]);
    anyhow::ensure!(
        stats.helper_starts == 1,
        "{operation} expected exactly one apply_patch-scoped fs helper, got {}",
        stats.helper_starts
    );
    anyhow::ensure!(
        stats.setup_refreshes == 1,
        "{operation} expected exactly one sandbox setup refresh, got {}",
        stats.setup_refreshes
    );
    let setup_refresh_child_execution_ms = stats
        .setup_refresh_child_execution_ms
        .with_context(|| format!("missing setup refresh child execution timing for {operation}"))?;
    let sandbox_prepare_total_ms = stats
        .sandbox_prepare_total_ms
        .with_context(|| format!("missing elevated sandbox preparation timing for {operation}"))?;
    let sandbox_runner_launch_total_ms = stats
        .sandbox_runner_launch_total_ms
        .with_context(|| format!("missing sandbox runner timing for {operation}"))?;
    let result = BaselineResult {
        operation,
        elapsed_ms,
        fs_helper_starts: stats.helper_starts,
        setup_refreshes: stats.setup_refreshes,
        first_fs_request_ms,
        setup_refresh_total_ms: stats.setup_refresh_total_ms,
        setup_refresh_child_execution_ms,
        setup_refresh_orchestration_estimated_ms: stats.setup_refresh_total_ms
            - setup_refresh_child_execution_ms,
        sandbox_prepare_total_ms,
        sandbox_prepare_excluding_refresh_ms: sandbox_prepare_total_ms
            - stats.setup_refresh_total_ms,
        sandbox_runner_launch_total_ms,
        first_fs_unattributed_residual_ms: first_fs_request_ms
            - sandbox_prepare_total_ms
            - sandbox_runner_launch_total_ms,
    };
    println!(
        "APPLY_PATCH_BASELINE_RESULT {}",
        serde_json::to_string(&result).context("serialize baseline result")?
    );
    println!("APPLY_PATCH_BASELINE_END {operation}");
    Ok(lines.len())
}

fn first_fs_request_elapsed_ms(trace: &[u8], measurement_id: &str) -> Result<f64> {
    let trace = String::from_utf8_lossy(trace);
    trace
        .lines()
        .filter(|line| line.contains(measurement_id))
        .find(|line| line.contains("filesystem sandbox helper invocation completed"))
        .and_then(elapsed_ms_from_line)
        .context("correlated first filesystem helper completion was not present in tracing output")
}

fn summarize_sandbox_log(lines: &[&str]) -> SandboxLogStats {
    let mut stats = SandboxLogStats::default();
    for line in lines {
        if line.contains("START:") && line.contains(CODEX_FS_HELPER_ARG1) {
            stats.helper_starts += 1;
        }
        if line.contains("setup refresh: completed success=") {
            stats.setup_refreshes += 1;
            if let Some(value) = elapsed_ms_from_line(line) {
                stats.setup_refresh_total_ms += value;
            }
        } else if line.contains("setup refresh child execution: completed success=") {
            stats.setup_refresh_child_execution_ms = elapsed_ms_from_line(line);
        } else if line.contains("elevated sandbox preparation: completed success=") {
            stats.sandbox_prepare_total_ms = elapsed_ms_from_line(line);
        } else if line.contains("elevated sandbox runner launch: completed success=") {
            stats.sandbox_runner_launch_total_ms = elapsed_ms_from_line(line);
        }
    }
    stats
}

fn elapsed_ms_from_line(line: &str) -> Option<f64> {
    let (_, elapsed) = line.rsplit_once("elapsed_ms=")?;
    elapsed.split_whitespace().next()?.parse::<f64>().ok()
}
