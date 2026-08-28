use codex_exec_server_protocol::FsFilePreimage;
use codex_exec_server_protocol::FsMutateBatchParams;
use codex_exec_server_protocol::FsMutation;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;

#[test]
fn fs_mutate_batch_round_trips_with_limits_relevant_fields() -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let first = PathUri::from_host_native_path(root.join("first.txt"))?;
    let second = PathUri::from_host_native_path(root.join("second.txt"))?;
    let params = FsMutateBatchParams {
        mutations: vec![
            FsMutation::Write {
                path: first.clone(),
                expected: FsFilePreimage::Exact(b"before".to_vec().into()),
                contents: b"after".to_vec().into(),
            },
            FsMutation::Remove {
                path: second.clone(),
                expected: b"delete".to_vec().into(),
            },
        ],
        follow_symlinks: Some(false),
        sandbox: None,
    };

    let value = serde_json::to_value(&params)?;
    assert_eq!(value["followSymlinks"], false);
    assert_eq!(value["mutations"][0]["type"], "write");
    assert_eq!(value["mutations"][0]["path"], first.to_string());
    assert_eq!(value["mutations"][1]["type"], "remove");
    assert_eq!(value["mutations"][1]["path"], second.to_string());

    let round_trip: FsMutateBatchParams = serde_json::from_value(value)?;
    assert_eq!(round_trip, params);
    Ok(())
}
