from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise RuntimeError(f"{label}: expected one match, got {count}")
    return text.replace(old, new, 1)


path = Path("codex-rs/exec-server/src/fs_helper_main.rs")
text = path.read_text()
text = replace_once(
    text,
    "pub fn main() -> ! {\n    let exit_code = match tokio::runtime::Builder::new_current_thread()",
    '''pub fn main() -> ! {
    eprintln!("FSHDBG main:enter");
    let exit_code = match tokio::runtime::Builder::new_current_thread()''',
    "helper main entry",
)
text = replace_once(
    text,
    "async fn run_main() -> Result<(), Box<dyn Error + Send + Sync>> {\n    let mut stdin = BufReader::new(io::stdin());",
    '''async fn run_main() -> Result<(), Box<dyn Error + Send + Sync>> {
    eprintln!("FSHDBG run_main:enter");
    let mut stdin = BufReader::new(io::stdin());''',
    "run_main entry",
)
text = replace_once(
    text,
    "    stdin.read_line(&mut input).await?;\n    let request: FsHelperRequest = serde_json::from_str(&input)?;",
    '''    eprintln!("FSHDBG stdin:read:start");
    stdin.read_line(&mut input).await?;
    eprintln!("FSHDBG stdin:read:done bytes={}", input.len());
    eprintln!("FSHDBG serde:start");
    let request: FsHelperRequest = serde_json::from_str(&input)?;
    let request_name = match &request {
        FsHelperRequest::Open(_) => "open",
        FsHelperRequest::ReadFile(_) => "readFile",
        FsHelperRequest::WriteFile(_) => "writeFile",
        FsHelperRequest::CreateDirectory(_) => "createDirectory",
        FsHelperRequest::GetMetadata(_) => "getMetadata",
        FsHelperRequest::Canonicalize(_) => "canonicalize",
        FsHelperRequest::ReadDirectory(_) => "readDirectory",
        FsHelperRequest::Walk(_) => "walk",
        FsHelperRequest::Remove(_) => "remove",
        FsHelperRequest::Copy(_) => "copy",
        FsHelperRequest::MutateBatch(_) => "mutateBatch",
    };
    eprintln!("FSHDBG serde:done op={request_name}");''',
    "stdin serde boundary",
)
text = replace_once(
    text,
    "        request => run_direct_request(request).await,",
    '''        request => {
            eprintln!("FSHDBG dispatch:start op={request_name}");
            let result = run_direct_request(request).await;
            eprintln!("FSHDBG dispatch:done op={request_name} ok={}", result.is_ok());
            result
        },''',
    "direct dispatch boundary",
)
text = replace_once(
    text,
    "    let response = match result {\n        Ok(payload) => FsHelperResponse::Ok(payload),",
    '''    eprintln!("FSHDBG response:build:start");
    let response = match result {
        Ok(payload) => FsHelperResponse::Ok(payload),''',
    "response build",
)
text = replace_once(
    text,
    "    let mut stdout = io::stdout();\n    stdout\n        .write_all(serde_json::to_string(&response)?.as_bytes())",
    '''    eprintln!("FSHDBG response:build:done");
    let mut stdout = io::stdout();
    eprintln!("FSHDBG response:serialize-write:start");
    stdout
        .write_all(serde_json::to_string(&response)?.as_bytes())''',
    "response serialization",
)
text = replace_once(
    text,
    "    stdout.flush().await?;\n",
    '''    stdout.flush().await?;
    eprintln!("FSHDBG response:serialize-write:done");
''',
    "response write done",
)
path.write_text(text)

path = Path("codex-rs/exec-server/src/fs_helper.rs")
text = path.read_text()
text = replace_once(
    text,
    "        FsHelperRequest::MutateBatch(params) => crate::server::mutate_batch(&file_system, params)\n            .await\n            .map(FsHelperPayload::MutateBatch),",
    '''        FsHelperRequest::MutateBatch(params) => {
            eprintln!("FSHDBG mutate-arm:enter ops={} follow={:?}", params.mutations.len(), params.follow_symlinks);
            let result = crate::server::mutate_batch(&file_system, params).await;
            eprintln!("FSHDBG mutate-arm:server-return ok={}", result.is_ok());
            result.map(FsHelperPayload::MutateBatch)
        },''',
    "mutate helper arm",
)
path.write_text(text)

# Add server-side boundary markers before the transaction engine.
path = Path("codex-rs/exec-server/src/server/file_system_handler.rs")
text = path.read_text()
text = replace_once(
    text,
    "pub(crate) async fn mutate_batch(\n    file_system: &dyn ExecutorFileSystem,\n    params: FsMutateBatchParams,\n) -> Result<FsMutateBatchResponse, JSONRPCErrorError> {\n    if params.mutations.len() > MAX_FS_MUTATE_BATCH_OPERATIONS {",
    '''pub(crate) async fn mutate_batch(
    file_system: &dyn ExecutorFileSystem,
    params: FsMutateBatchParams,
) -> Result<FsMutateBatchResponse, JSONRPCErrorError> {
    eprintln!("FSHDBG server-mutate:enter ops={} follow={:?}", params.mutations.len(), params.follow_symlinks);
    if params.mutations.len() > MAX_FS_MUTATE_BATCH_OPERATIONS {''',
    "server mutate entry",
)
text = replace_once(
    text,
    "    let batch = FileMutationBatch {\n        mutations: params.mutations.into_iter().map(Into::into).collect(),",
    '''    eprintln!("FSHDBG server-mutate:limits-ok");
    let batch = FileMutationBatch {
        mutations: params.mutations.into_iter().map(Into::into).collect(),''',
    "server limits done",
)
text = replace_once(
    text,
    "    let outcome = file_system\n        .mutate_batch(batch, params.sandbox.as_ref())",
    '''    eprintln!("FSHDBG server-mutate:engine-call");
    let outcome = file_system
        .mutate_batch(batch, params.sandbox.as_ref())''',
    "server engine call",
)
text = replace_once(
    text,
    "        .await\n        .map_err(map_fs_error)?;\n    Ok(FsMutateBatchResponse {",
    '''        .await
        .map_err(map_fs_error)?;
    eprintln!("FSHDBG server-mutate:engine-return outcome={outcome:?}");
    Ok(FsMutateBatchResponse {''',
    "server engine return",
)
path.write_text(text)
