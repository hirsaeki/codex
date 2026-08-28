use std::collections::HashMap;
use std::io;

use codex_exec_server::ExecutorFileSystem;
use codex_exec_server::FileMutation;
use codex_exec_server::FileMutationBatch;
use codex_exec_server::FilePreimage;
use codex_exec_server::FileSystemSandboxContext;
use codex_exec_server::GetMetadataOptions;
use codex_exec_server::ReadFileOptions;
use codex_utils_path_uri::PathUri;

use crate::AffectedPaths;
use crate::AppliedPatchChange;
use crate::AppliedPatchDelta;
use crate::AppliedPatchFileChange;
use crate::ApplyPatchError;
use crate::ApplyPatchOptions;
use crate::IoError;
use crate::parser::Hunk;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreparedPatch {
    pub(crate) batch: FileMutationBatch,
    pub(crate) delta: AppliedPatchDelta,
    pub(crate) affected: AffectedPaths,
}

#[derive(Clone)]
struct OverlayEntry {
    initial: FilePreimage,
    current: Option<Vec<u8>>,
    missing_error: Option<(io::ErrorKind, String)>,
}

pub(crate) async fn prepare_hunks(
    hunks: &[Hunk],
    options: ApplyPatchOptions,
    cwd: &PathUri,
    fs: &dyn ExecutorFileSystem,
    sandbox: Option<&FileSystemSandboxContext>,
) -> Result<PreparedPatch, ApplyPatchError> {
    let mut overlay = HashMap::<PathUri, OverlayEntry>::new();
    let mut path_order = Vec::new();
    let mut delta = AppliedPatchDelta::empty();
    let mut affected = AffectedPaths::default();

    for hunk in hunks {
        let source_path = hunk.resolve_path(cwd)?;
        load_path(
            &source_path,
            &mut overlay,
            &mut path_order,
            fs,
            options.follow_symlinks,
            sandbox,
        )
        .await?;
        note_existing_path_delta_support(
            &source_path,
            fs,
            options.follow_symlinks,
            sandbox,
            &mut delta.exact,
        )
        .await;
        let affected_path = hunk.path().to_path_buf();

        match hunk {
            Hunk::AddFile { contents, .. } => {
                let overwritten_content = overlay[&source_path]
                    .current
                    .as_deref()
                    .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok());
                if overlay[&source_path].current.is_some() && overwritten_content.is_none() {
                    delta.exact = false;
                }
                overlay.get_mut(&source_path).expect("loaded path").current =
                    Some(contents.clone().into_bytes());
                delta.changes.push(AppliedPatchChange {
                    path: source_path,
                    change: AppliedPatchFileChange::Add {
                        content: contents.clone(),
                        overwritten_content,
                    },
                });
                affected.added.push(affected_path);
            }
            Hunk::DeleteFile { .. } => {
                let bytes = current_bytes(&source_path, &overlay, "delete")?;
                let content = match String::from_utf8(bytes) {
                    Ok(content) => Some(content),
                    Err(_) => {
                        delta.exact = false;
                        None
                    }
                };
                overlay.get_mut(&source_path).expect("loaded path").current = None;
                if let Some(content) = content {
                    delta.changes.push(AppliedPatchChange {
                        path: source_path,
                        change: AppliedPatchFileChange::Delete { content },
                    });
                }
                affected.deleted.push(affected_path);
            }
            Hunk::UpdateFile {
                move_path, chunks, ..
            } => {
                let original_content = current_text(&source_path, &overlay, "update")?;
                let update = crate::file_update::derive_new_contents_from_contents(
                    &source_path,
                    original_content,
                    chunks,
                    options.update_file_mode,
                )?;
                let destination = move_path
                    .as_ref()
                    .map(|path| cwd.join(&path.to_string_lossy()))
                    .transpose()?;
                if destination.as_ref() == Some(&source_path) {
                    return Err(crate::ParseError::InvalidPatchError(format!(
                        "move destination is the same as source {}",
                        source_path.inferred_native_path_string()
                    ))
                    .into());
                }
                let overwritten_move_content = if let Some(destination) = &destination {
                    load_path(
                        destination,
                        &mut overlay,
                        &mut path_order,
                        fs,
                        options.follow_symlinks,
                        sandbox,
                    )
                    .await?;
                    note_existing_path_delta_support(
                        destination,
                        fs,
                        options.follow_symlinks,
                        sandbox,
                        &mut delta.exact,
                    )
                    .await;
                    let content = overlay[destination]
                        .current
                        .as_deref()
                        .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok());
                    if overlay[destination].current.is_some() && content.is_none() {
                        delta.exact = false;
                    }
                    overlay.get_mut(destination).expect("loaded path").current =
                        Some(update.new_contents.clone().into_bytes());
                    overlay.get_mut(&source_path).expect("loaded path").current = None;
                    content
                } else {
                    overlay.get_mut(&source_path).expect("loaded path").current =
                        Some(update.new_contents.clone().into_bytes());
                    None
                };
                delta.changes.push(AppliedPatchChange {
                    path: source_path,
                    change: AppliedPatchFileChange::Update {
                        move_path: destination,
                        old_content: update.original_contents,
                        overwritten_move_content,
                        new_content: update.new_contents,
                    },
                });
                affected.modified.push(affected_path);
            }
        }
    }

    let mutations = path_order
        .into_iter()
        .filter_map(|path| {
            let entry = overlay.remove(&path)?;
            if matches!((&entry.initial, &entry.current), (FilePreimage::Missing, None))
                || matches!((&entry.initial, &entry.current), (FilePreimage::Exact(initial), Some(current)) if initial == current)
            {
                return None;
            }
            match entry.current {
                Some(contents) => Some(FileMutation::Write {
                    path,
                    expected: entry.initial,
                    contents,
                }),
                None => match entry.initial {
                    FilePreimage::Exact(expected) => {
                        Some(FileMutation::Remove { path, expected })
                    }
                    FilePreimage::Missing => None,
                },
            }
        })
        .collect();

    Ok(PreparedPatch {
        batch: FileMutationBatch {
            mutations,
            follow_symlinks: options.follow_symlinks,
        },
        delta,
        affected,
    })
}

async fn load_path(
    path: &PathUri,
    overlay: &mut HashMap<PathUri, OverlayEntry>,
    path_order: &mut Vec<PathUri>,
    fs: &dyn ExecutorFileSystem,
    follow_symlinks: bool,
    sandbox: Option<&FileSystemSandboxContext>,
) -> Result<(), ApplyPatchError> {
    if overlay.contains_key(path) {
        return Ok(());
    }
    let (initial, current, missing_error) = match fs
        .read_file(path, ReadFileOptions { follow_symlinks }, sandbox)
        .await
    {
        Ok(bytes) => (FilePreimage::Exact(bytes.clone()), Some(bytes), None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (
            FilePreimage::Missing,
            None,
            Some((error.kind(), error.to_string())),
        ),
        Err(source) => {
            return Err(ApplyPatchError::IoError(IoError {
                context: format!("Failed to read {}", path.inferred_native_path_string()),
                source,
            }));
        }
    };
    overlay.insert(
        path.clone(),
        OverlayEntry {
            initial,
            current,
            missing_error,
        },
    );
    path_order.push(path.clone());
    Ok(())
}

async fn note_existing_path_delta_support(
    path: &PathUri,
    fs: &dyn ExecutorFileSystem,
    follow_symlinks: bool,
    sandbox: Option<&FileSystemSandboxContext>,
    exact: &mut bool,
) {
    match fs
        .get_metadata(path, GetMetadataOptions { follow_symlinks }, sandbox)
        .await
    {
        Ok(metadata) if metadata.is_file && !metadata.is_symlink => {}
        Ok(_) => *exact = false,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(_) => *exact = false,
    }
}

fn current_bytes(
    path: &PathUri,
    overlay: &HashMap<PathUri, OverlayEntry>,
    operation: &str,
) -> Result<Vec<u8>, ApplyPatchError> {
    let entry = &overlay[path];
    entry.current.clone().ok_or_else(|| {
        ApplyPatchError::IoError(IoError {
            context: format!(
                "Failed to read file to {operation} {}",
                path.inferred_native_path_string()
            ),
            source: entry
                .missing_error
                .as_ref()
                .map(|(kind, message)| io::Error::new(*kind, message.clone()))
                .unwrap_or_else(|| io::Error::from(io::ErrorKind::NotFound)),
        })
    })
}

fn current_text(
    path: &PathUri,
    overlay: &HashMap<PathUri, OverlayEntry>,
    operation: &str,
) -> Result<String, ApplyPatchError> {
    String::from_utf8(current_bytes(path, overlay, operation)?).map_err(|error| {
        ApplyPatchError::IoError(IoError {
            context: format!(
                "Failed to read file to {operation} {}",
                path.inferred_native_path_string()
            ),
            source: io::Error::new(io::ErrorKind::InvalidData, error),
        })
    })
}
