use super::*;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

fn path_uri(path: &Path) -> PathUri {
    PathUri::from_host_native_path(path).expect("temporary path should be absolute")
}

fn write(path: &Path, expected: FilePreimage, contents: &[u8]) -> FileMutation {
    FileMutation::Write {
        path: path_uri(path),
        expected,
        contents: contents.to_vec(),
    }
}

fn remove(path: &Path, expected: &[u8]) -> FileMutation {
    FileMutation::Remove {
        path: path_uri(path),
        expected: expected.to_vec(),
    }
}

#[test]
fn commits_mixed_mutations() -> io::Result<()> {
    let temp = TempDir::new()?;
    let added = temp.path().join("nested/added.txt");
    let updated = temp.path().join("updated.txt");
    let removed = temp.path().join("removed.txt");
    std::fs::write(&updated, b"before")?;
    std::fs::write(&removed, b"delete")?;

    let outcome = mutate_batch(FileMutationBatch {
        mutations: vec![
            write(&added, FilePreimage::Missing, b"added"),
            write(&updated, FilePreimage::Exact(b"before".to_vec()), b"after"),
            remove(&removed, b"delete"),
        ],
    });

    assert_eq!(outcome, FileMutationBatchOutcome::Committed);
    assert_eq!(std::fs::read(added)?, b"added");
    assert_eq!(std::fs::read(updated)?, b"after");
    assert!(!removed.exists());
    Ok(())
}

#[test]
fn stale_preimage_rejects_without_mutation() -> io::Result<()> {
    let temp = TempDir::new()?;
    let first = temp.path().join("first.txt");
    let stale = temp.path().join("stale.txt");
    std::fs::write(&first, b"first")?;
    std::fs::write(&stale, b"current")?;

    let outcome = mutate_batch(FileMutationBatch {
        mutations: vec![
            write(&first, FilePreimage::Exact(b"first".to_vec()), b"changed"),
            write(&stale, FilePreimage::Exact(b"old".to_vec()), b"new"),
        ],
    });

    assert!(matches!(outcome, FileMutationBatchOutcome::Rejected { .. }));
    assert_eq!(std::fs::read(first)?, b"first");
    assert_eq!(std::fs::read(stale)?, b"current");
    Ok(())
}

#[test]
fn rejects_operation_limit_without_mutation() -> io::Result<()> {
    let temp = TempDir::new()?;
    let first = temp.path().join("first.txt");
    let second = temp.path().join("second.txt");
    std::fs::write(&first, b"first")?;
    std::fs::write(&second, b"second")?;

    let result = preflight_with_limits(
        FileMutationBatch {
            mutations: vec![
                write(&first, FilePreimage::Exact(b"first".to_vec()), b"changed"),
                write(&second, FilePreimage::Exact(b"second".to_vec()), b"changed"),
            ],
        },
        1,
        usize::MAX,
    );

    assert!(result.is_err());
    assert_eq!(std::fs::read(first)?, b"first");
    assert_eq!(std::fs::read(second)?, b"second");
    Ok(())
}

#[test]
fn rejects_request_byte_limit_without_mutation() -> io::Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("file.txt");

    let result = preflight_with_limits(
        FileMutationBatch {
            mutations: vec![write(&path, FilePreimage::Missing, b"12345")],
        },
        usize::MAX,
        4,
    );

    assert!(result.is_err());
    assert!(!path.exists());
    Ok(())
}

#[test]
fn rejects_preimage_bytes_over_remaining_limit_without_mutation() -> io::Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("file.txt");
    std::fs::write(&path, b"12345678")?;

    let result = preflight_with_limits(
        FileMutationBatch {
            mutations: vec![write(&path, FilePreimage::Exact(b"12345678".to_vec()), b"")],
        },
        usize::MAX,
        12,
    );

    assert!(result.is_err());
    assert_eq!(std::fs::read(path)?, b"12345678");
    Ok(())
}

#[test]
fn rejects_duplicate_target_without_mutation() -> io::Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("file.txt");
    std::fs::write(&path, b"before")?;

    let outcome = mutate_batch(FileMutationBatch {
        mutations: vec![
            write(&path, FilePreimage::Exact(b"before".to_vec()), b"first"),
            write(&path, FilePreimage::Exact(b"before".to_vec()), b"second"),
        ],
    });

    assert!(matches!(outcome, FileMutationBatchOutcome::Rejected { .. }));
    assert_eq!(std::fs::read(path)?, b"before");
    Ok(())
}

#[cfg(unix)]
#[test]
fn rejects_symlink_in_existing_ancestor_chain() -> io::Result<()> {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new()?;
    let real = temp.path().join("real");
    let link = temp.path().join("link");
    std::fs::create_dir(&real)?;
    symlink(&real, &link)?;
    let target = link.join("child/file.txt");

    let outcome = mutate_batch(FileMutationBatch {
        mutations: vec![write(&target, FilePreimage::Missing, b"contents")],
    });

    assert!(matches!(outcome, FileMutationBatchOutcome::Rejected { .. }));
    assert!(!real.join("child").exists());
    Ok(())
}

#[test]
fn injected_failure_after_each_step_restores_files_and_directories() -> io::Result<()> {
    for fail_after in 0..3 {
        let temp = TempDir::new()?;
        let added = temp.path().join("new/child/added.txt");
        let updated = temp.path().join("updated.txt");
        let removed = temp.path().join("removed.txt");
        std::fs::write(&updated, b"before")?;
        std::fs::write(&removed, b"delete")?;
        let outcome = mutate_batch_with_hook(
            FileMutationBatch {
                mutations: vec![
                    write(&added, FilePreimage::Missing, b"added"),
                    write(&updated, FilePreimage::Exact(b"before".to_vec()), b"after"),
                    remove(&removed, b"delete"),
                ],
            },
            |checkpoint| match checkpoint {
                Checkpoint::AfterMutation(index) if index == fail_after => {
                    Err(io::Error::other("injected failure"))
                }
                Checkpoint::AfterPreflight
                | Checkpoint::BeforeWriteMutation(_)
                | Checkpoint::AfterWriteTruncate(_)
                | Checkpoint::BeforeWritePublish(_)
                | Checkpoint::AfterWritePublish(_)
                | Checkpoint::BeforeRemove(_)
                | Checkpoint::BeforeRemoveRename(_)
                | Checkpoint::AfterMutation(_)
                | Checkpoint::BeforeRollback(_)
                | Checkpoint::BeforeRollbackRemoveRename(_)
                | Checkpoint::BeforeRollbackRestoreRename(_)
                | Checkpoint::BeforeCreatedDirectoryRename(_) => Ok(()),
            },
        );

        assert_eq!(
            outcome,
            FileMutationBatchOutcome::RolledBack {
                error: "injected failure".to_string()
            }
        );
        assert!(!added.exists());
        assert!(!temp.path().join("new").exists());
        assert_eq!(std::fs::read(updated)?, b"before");
        assert_eq!(std::fs::read(removed)?, b"delete");
    }
    Ok(())
}

#[test]
fn failure_after_truncate_restores_file() -> io::Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("file.txt");
    std::fs::write(&path, b"before")?;

    let outcome = mutate_batch_with_hook(
        FileMutationBatch {
            mutations: vec![write(
                &path,
                FilePreimage::Exact(b"before".to_vec()),
                b"after",
            )],
        },
        |checkpoint| match checkpoint {
            Checkpoint::AfterWriteTruncate(0) => Err(io::Error::other("injected write failure")),
            Checkpoint::AfterPreflight
            | Checkpoint::BeforeWriteMutation(_)
            | Checkpoint::AfterWriteTruncate(_)
            | Checkpoint::BeforeWritePublish(_)
            | Checkpoint::AfterWritePublish(_)
            | Checkpoint::BeforeRemove(_)
            | Checkpoint::BeforeRemoveRename(_)
            | Checkpoint::AfterMutation(_)
            | Checkpoint::BeforeRollback(_)
            | Checkpoint::BeforeRollbackRemoveRename(_)
            | Checkpoint::BeforeRollbackRestoreRename(_)
            | Checkpoint::BeforeCreatedDirectoryRename(_) => Ok(()),
        },
    );

    assert_eq!(
        outcome,
        FileMutationBatchOutcome::RolledBack {
            error: format!(
                "filesystem mutation 0 for `{}` failed: injected write failure",
                path.display()
            )
        }
    );
    assert_eq!(std::fs::read(path)?, b"before");
    Ok(())
}

#[test]
fn replacement_before_write_is_not_overwritten() -> io::Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("file.txt");
    let replacement = temp.path().join("replacement.txt");
    std::fs::write(&path, b"before")?;
    std::fs::write(&replacement, b"external")?;

    let outcome = mutate_batch_with_hook(
        FileMutationBatch {
            mutations: vec![write(
                &path,
                FilePreimage::Exact(b"before".to_vec()),
                b"after",
            )],
        },
        |checkpoint| match checkpoint {
            Checkpoint::BeforeWriteMutation(0) => {
                std::fs::remove_file(&path)?;
                std::fs::rename(&replacement, &path)
            }
            Checkpoint::AfterPreflight
            | Checkpoint::BeforeWriteMutation(_)
            | Checkpoint::AfterWriteTruncate(_)
            | Checkpoint::BeforeWritePublish(_)
            | Checkpoint::AfterWritePublish(_)
            | Checkpoint::BeforeRemove(_)
            | Checkpoint::BeforeRemoveRename(_)
            | Checkpoint::AfterMutation(_)
            | Checkpoint::BeforeRollback(_)
            | Checkpoint::BeforeRollbackRemoveRename(_)
            | Checkpoint::BeforeRollbackRestoreRename(_)
            | Checkpoint::BeforeCreatedDirectoryRename(_) => Ok(()),
        },
    );

    assert!(matches!(
        outcome,
        FileMutationBatchOutcome::RolledBack { .. }
    ));
    assert_eq!(std::fs::read(path)?, b"external");
    Ok(())
}

#[test]
fn replacement_before_remove_is_not_deleted() -> io::Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("file.txt");
    let replacement = temp.path().join("replacement.txt");
    std::fs::write(&path, b"before")?;
    std::fs::write(&replacement, b"external")?;

    let outcome = mutate_batch_with_hook(
        FileMutationBatch {
            mutations: vec![remove(&path, b"before")],
        },
        |checkpoint| match checkpoint {
            Checkpoint::BeforeRemove(0) => {
                std::fs::remove_file(&path)?;
                std::fs::rename(&replacement, &path)
            }
            Checkpoint::AfterPreflight
            | Checkpoint::BeforeWriteMutation(_)
            | Checkpoint::AfterWriteTruncate(_)
            | Checkpoint::BeforeWritePublish(_)
            | Checkpoint::AfterWritePublish(_)
            | Checkpoint::BeforeRemove(_)
            | Checkpoint::BeforeRemoveRename(_)
            | Checkpoint::AfterMutation(_)
            | Checkpoint::BeforeRollback(_)
            | Checkpoint::BeforeRollbackRemoveRename(_)
            | Checkpoint::BeforeRollbackRestoreRename(_)
            | Checkpoint::BeforeCreatedDirectoryRename(_) => Ok(()),
        },
    );

    assert!(matches!(
        outcome,
        FileMutationBatchOutcome::RolledBack { .. }
    ));
    assert_eq!(std::fs::read(path)?, b"external");
    Ok(())
}

#[test]
fn replacement_after_final_remove_check_is_restored() -> io::Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("file.txt");
    let replacement = temp.path().join("replacement.txt");
    std::fs::write(&path, b"before")?;
    std::fs::write(&replacement, b"external")?;

    let outcome = mutate_batch_with_hook(
        FileMutationBatch {
            mutations: vec![remove(&path, b"before")],
        },
        |checkpoint| match checkpoint {
            Checkpoint::BeforeRemoveRename(0) => {
                std::fs::remove_file(&path)?;
                std::fs::rename(&replacement, &path)
            }
            Checkpoint::AfterPreflight
            | Checkpoint::BeforeWriteMutation(_)
            | Checkpoint::AfterWriteTruncate(_)
            | Checkpoint::BeforeWritePublish(_)
            | Checkpoint::AfterWritePublish(_)
            | Checkpoint::BeforeRemove(_)
            | Checkpoint::BeforeRemoveRename(_)
            | Checkpoint::AfterMutation(_)
            | Checkpoint::BeforeRollback(_)
            | Checkpoint::BeforeRollbackRemoveRename(_)
            | Checkpoint::BeforeRollbackRestoreRename(_)
            | Checkpoint::BeforeCreatedDirectoryRename(_) => Ok(()),
        },
    );

    assert!(matches!(
        outcome,
        FileMutationBatchOutcome::RolledBack { .. }
    ));
    assert_eq!(std::fs::read(path)?, b"external");
    Ok(())
}

#[test]
fn replacement_after_final_rollback_remove_check_is_not_deleted() -> io::Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("file.txt");
    let replacement = temp.path().join("replacement.txt");
    std::fs::write(&replacement, b"external")?;

    let outcome = mutate_batch_with_hook(
        FileMutationBatch {
            mutations: vec![write(&path, FilePreimage::Missing, b"batch")],
        },
        |checkpoint| match checkpoint {
            Checkpoint::AfterMutation(0) => Err(io::Error::other("injected failure")),
            Checkpoint::BeforeRollbackRemoveRename(0) => {
                std::fs::remove_file(&path)?;
                std::fs::rename(&replacement, &path)
            }
            Checkpoint::AfterPreflight
            | Checkpoint::BeforeWriteMutation(_)
            | Checkpoint::AfterWriteTruncate(_)
            | Checkpoint::BeforeWritePublish(_)
            | Checkpoint::AfterWritePublish(_)
            | Checkpoint::BeforeRemove(_)
            | Checkpoint::BeforeRemoveRename(_)
            | Checkpoint::AfterMutation(_)
            | Checkpoint::BeforeRollback(_)
            | Checkpoint::BeforeRollbackRemoveRename(_)
            | Checkpoint::BeforeRollbackRestoreRename(_)
            | Checkpoint::BeforeCreatedDirectoryRename(_) => Ok(()),
        },
    );

    assert!(matches!(
        outcome,
        FileMutationBatchOutcome::Indeterminate {
            possibly_mutated_paths,
            ..
        } if possibly_mutated_paths == vec![path_uri(&path)]
    ));
    assert_eq!(std::fs::read(path)?, b"external");
    Ok(())
}

#[cfg(unix)]
#[test]
fn parent_symlink_swap_before_write_is_not_followed() -> io::Result<()> {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new()?;
    let parent = temp.path().join("parent");
    let moved_parent = temp.path().join("moved-parent");
    let external = temp.path().join("external");
    std::fs::create_dir(&parent)?;
    std::fs::create_dir(&external)?;
    let path = parent.join("file.txt");
    let external_path = external.join("file.txt");
    std::fs::write(&path, b"before")?;
    std::fs::write(&external_path, b"external")?;

    let outcome = mutate_batch_with_hook(
        FileMutationBatch {
            mutations: vec![write(
                &path,
                FilePreimage::Exact(b"before".to_vec()),
                b"after",
            )],
        },
        |checkpoint| match checkpoint {
            Checkpoint::BeforeWriteMutation(0) => {
                std::fs::rename(&parent, &moved_parent)?;
                symlink(&external, &parent)
            }
            Checkpoint::AfterPreflight
            | Checkpoint::BeforeWriteMutation(_)
            | Checkpoint::AfterWriteTruncate(_)
            | Checkpoint::BeforeWritePublish(_)
            | Checkpoint::AfterWritePublish(_)
            | Checkpoint::BeforeRemove(_)
            | Checkpoint::BeforeRemoveRename(_)
            | Checkpoint::AfterMutation(_)
            | Checkpoint::BeforeRollback(_)
            | Checkpoint::BeforeRollbackRemoveRename(_)
            | Checkpoint::BeforeRollbackRestoreRename(_)
            | Checkpoint::BeforeCreatedDirectoryRename(_) => Ok(()),
        },
    );

    assert!(matches!(
        outcome,
        FileMutationBatchOutcome::RolledBack { .. }
    ));
    assert_eq!(std::fs::read(external_path)?, b"external");
    Ok(())
}

#[cfg(unix)]
#[test]
fn parent_symlink_swap_before_missing_write_publish_is_not_followed() -> io::Result<()> {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new()?;
    let parent = temp.path().join("parent");
    let moved_parent = temp.path().join("moved-parent");
    let external = temp.path().join("external");
    std::fs::create_dir(&parent)?;
    std::fs::create_dir(&external)?;
    let path = parent.join("file.txt");
    let external_path = external.join("file.txt");
    std::fs::write(&external_path, b"external")?;

    let outcome = mutate_batch_with_hook(
        FileMutationBatch {
            mutations: vec![write(&path, FilePreimage::Missing, b"batch")],
        },
        |checkpoint| match checkpoint {
            Checkpoint::BeforeWritePublish(0) => {
                std::fs::rename(&parent, &moved_parent)?;
                symlink(&external, &parent)
            }
            Checkpoint::AfterPreflight
            | Checkpoint::BeforeWriteMutation(_)
            | Checkpoint::AfterWriteTruncate(_)
            | Checkpoint::BeforeWritePublish(_)
            | Checkpoint::AfterWritePublish(_)
            | Checkpoint::BeforeRemove(_)
            | Checkpoint::BeforeRemoveRename(_)
            | Checkpoint::AfterMutation(_)
            | Checkpoint::BeforeRollback(_)
            | Checkpoint::BeforeRollbackRemoveRename(_)
            | Checkpoint::BeforeRollbackRestoreRename(_)
            | Checkpoint::BeforeCreatedDirectoryRename(_) => Ok(()),
        },
    );

    assert!(matches!(
        outcome,
        FileMutationBatchOutcome::Indeterminate { .. }
    ));
    assert_eq!(std::fs::read(external_path)?, b"external");
    Ok(())
}

#[test]
fn replacement_after_missing_write_publish_is_not_deleted() -> io::Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("file.txt");
    let replacement = temp.path().join("replacement.txt");
    std::fs::write(&replacement, b"external")?;

    let outcome = mutate_batch_with_hook(
        FileMutationBatch {
            mutations: vec![write(&path, FilePreimage::Missing, b"batch")],
        },
        |checkpoint| match checkpoint {
            Checkpoint::AfterWritePublish(0) => {
                std::fs::remove_file(&path)?;
                std::fs::rename(&replacement, &path)
            }
            Checkpoint::AfterPreflight
            | Checkpoint::BeforeWriteMutation(_)
            | Checkpoint::AfterWriteTruncate(_)
            | Checkpoint::BeforeWritePublish(_)
            | Checkpoint::AfterWritePublish(_)
            | Checkpoint::BeforeRemove(_)
            | Checkpoint::BeforeRemoveRename(_)
            | Checkpoint::AfterMutation(_)
            | Checkpoint::BeforeRollback(_)
            | Checkpoint::BeforeRollbackRemoveRename(_)
            | Checkpoint::BeforeRollbackRestoreRename(_)
            | Checkpoint::BeforeCreatedDirectoryRename(_) => Ok(()),
        },
    );

    assert!(matches!(
        outcome,
        FileMutationBatchOutcome::Indeterminate {
            possibly_mutated_paths,
            ..
        } if possibly_mutated_paths == vec![path_uri(&path)]
    ));
    assert_eq!(std::fs::read(path)?, b"external");
    Ok(())
}

#[test]
fn rollback_failure_is_indeterminate_and_does_not_stop_later_restores() -> io::Result<()> {
    let temp = TempDir::new()?;
    let first = temp.path().join("first.txt");
    let second = temp.path().join("second.txt");
    std::fs::write(&first, b"first before")?;
    std::fs::write(&second, b"second before")?;

    let outcome = mutate_batch_with_hook(
        FileMutationBatch {
            mutations: vec![
                write(
                    &first,
                    FilePreimage::Exact(b"first before".to_vec()),
                    b"first after",
                ),
                write(
                    &second,
                    FilePreimage::Exact(b"second before".to_vec()),
                    b"second after",
                ),
            ],
        },
        |checkpoint| match checkpoint {
            Checkpoint::AfterMutation(1) => Err(io::Error::other("injected commit failure")),
            Checkpoint::BeforeRollback(0) => Err(io::Error::other("injected rollback failure")),
            Checkpoint::AfterPreflight
            | Checkpoint::BeforeWriteMutation(_)
            | Checkpoint::AfterWriteTruncate(_)
            | Checkpoint::BeforeWritePublish(_)
            | Checkpoint::AfterWritePublish(_)
            | Checkpoint::BeforeRemove(_)
            | Checkpoint::BeforeRemoveRename(_)
            | Checkpoint::AfterMutation(_)
            | Checkpoint::BeforeRollback(_)
            | Checkpoint::BeforeRollbackRemoveRename(_)
            | Checkpoint::BeforeRollbackRestoreRename(_)
            | Checkpoint::BeforeCreatedDirectoryRename(_) => Ok(()),
        },
    );

    assert_eq!(
        outcome,
        FileMutationBatchOutcome::Indeterminate {
            error: format!(
                "injected commit failure; rollback failed: {}: injected rollback failure",
                second.display()
            ),
            possibly_mutated_paths: vec![path_uri(&second)],
        }
    );
    assert_eq!(std::fs::read(first)?, b"first before");
    assert_eq!(std::fs::read(second)?, b"second after");
    Ok(())
}

#[test]
fn cleanup_failure_reports_every_committed_path() -> io::Result<()> {
    let temp = TempDir::new()?;
    let first = temp.path().join("first.txt");
    let second = temp.path().join("second.txt");
    std::fs::write(&first, b"first")?;
    std::fs::write(&second, b"second")?;

    let outcome = mutate_batch_with_hook(
        FileMutationBatch {
            mutations: vec![remove(&first, b"first"), remove(&second, b"second")],
        },
        |checkpoint| {
            if checkpoint == Checkpoint::AfterMutation(1) {
                let second_quarantine = std::fs::read_dir(temp.path())?
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .find(|path| std::fs::read(path).is_ok_and(|contents| contents == b"second"))
                    .ok_or_else(|| io::Error::other("second quarantine not found"))?;
                std::fs::write(second_quarantine, b"changed")?;
            }
            Ok(())
        },
    );

    assert!(matches!(
        outcome,
        FileMutationBatchOutcome::Indeterminate {
            possibly_mutated_paths,
            ..
        } if possibly_mutated_paths == vec![path_uri(&first), path_uri(&second)]
    ));
    assert!(!first.exists());
    assert!(!second.exists());
    Ok(())
}

#[test]
fn replaced_postimage_with_same_bytes_is_not_overwritten() -> io::Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("file.txt");
    let replacement = temp.path().join("replacement.txt");
    std::fs::write(&path, b"before")?;
    std::fs::write(&replacement, b"after")?;

    let outcome = mutate_batch_with_hook(
        FileMutationBatch {
            mutations: vec![write(
                &path,
                FilePreimage::Exact(b"before".to_vec()),
                b"after",
            )],
        },
        |checkpoint| match checkpoint {
            Checkpoint::AfterMutation(0) => {
                std::fs::remove_file(&path)?;
                std::fs::rename(&replacement, &path)?;
                Err(io::Error::other("injected failure"))
            }
            Checkpoint::AfterPreflight
            | Checkpoint::BeforeWriteMutation(_)
            | Checkpoint::AfterWriteTruncate(_)
            | Checkpoint::BeforeWritePublish(_)
            | Checkpoint::AfterWritePublish(_)
            | Checkpoint::BeforeRemove(_)
            | Checkpoint::BeforeRemoveRename(_)
            | Checkpoint::AfterMutation(_)
            | Checkpoint::BeforeRollback(_)
            | Checkpoint::BeforeRollbackRemoveRename(_)
            | Checkpoint::BeforeRollbackRestoreRename(_)
            | Checkpoint::BeforeCreatedDirectoryRename(_) => Ok(()),
        },
    );

    assert!(matches!(
        outcome,
        FileMutationBatchOutcome::Indeterminate {
            possibly_mutated_paths,
            ..
        } if possibly_mutated_paths == vec![path_uri(&path)]
    ));
    assert_eq!(std::fs::read(path)?, b"after");
    Ok(())
}

#[test]
fn concurrent_postimage_change_returns_indeterminate() -> io::Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("file.txt");
    std::fs::write(&path, b"before")?;
    let outcome = mutate_batch_with_hook(
        FileMutationBatch {
            mutations: vec![write(
                &path,
                FilePreimage::Exact(b"before".to_vec()),
                b"after",
            )],
        },
        |checkpoint| match checkpoint {
            Checkpoint::AfterMutation(0) => {
                std::fs::write(&path, b"external")?;
                Err(io::Error::other("injected failure"))
            }
            Checkpoint::AfterPreflight
            | Checkpoint::BeforeWriteMutation(_)
            | Checkpoint::AfterWriteTruncate(_)
            | Checkpoint::BeforeWritePublish(_)
            | Checkpoint::AfterWritePublish(_)
            | Checkpoint::BeforeRemove(_)
            | Checkpoint::BeforeRemoveRename(_)
            | Checkpoint::AfterMutation(_)
            | Checkpoint::BeforeRollback(_)
            | Checkpoint::BeforeRollbackRemoveRename(_)
            | Checkpoint::BeforeRollbackRestoreRename(_)
            | Checkpoint::BeforeCreatedDirectoryRename(_) => Ok(()),
        },
    );

    assert!(matches!(
        outcome,
        FileMutationBatchOutcome::Indeterminate {
            possibly_mutated_paths,
            ..
        } if possibly_mutated_paths == vec![path_uri(&path)]
    ));
    assert_eq!(std::fs::read(path)?, b"external");
    Ok(())
}

#[cfg(unix)]
#[test]
fn permission_change_after_preflight_prevents_commit() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new()?;
    let path = temp.path().join("file.txt");
    std::fs::write(&path, b"before")?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
    let outcome = mutate_batch_with_hook(
        FileMutationBatch {
            mutations: vec![write(
                &path,
                FilePreimage::Exact(b"before".to_vec()),
                b"after",
            )],
        },
        |checkpoint| match checkpoint {
            Checkpoint::AfterPreflight => {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            }
            Checkpoint::BeforeWriteMutation(_)
            | Checkpoint::AfterWriteTruncate(_)
            | Checkpoint::BeforeWritePublish(_)
            | Checkpoint::AfterWritePublish(_)
            | Checkpoint::BeforeRemove(_)
            | Checkpoint::BeforeRemoveRename(_)
            | Checkpoint::AfterMutation(_)
            | Checkpoint::BeforeRollback(_)
            | Checkpoint::BeforeRollbackRemoveRename(_)
            | Checkpoint::BeforeRollbackRestoreRename(_)
            | Checkpoint::BeforeCreatedDirectoryRename(_) => Ok(()),
        },
    );

    assert!(matches!(
        outcome,
        FileMutationBatchOutcome::RolledBack { .. }
    ));
    assert_eq!(std::fs::read(&path)?, b"before");
    assert_eq!(std::fs::metadata(path)?.permissions().mode() & 0o777, 0o600);
    Ok(())
}

#[cfg(unix)]
#[test]
fn successful_overwrite_preserves_unix_mode() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new()?;
    let path = temp.path().join("executable");
    std::fs::write(&path, b"before")?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o4751))?;

    let outcome = mutate_batch(FileMutationBatch {
        mutations: vec![write(
            &path,
            FilePreimage::Exact(b"before".to_vec()),
            b"after",
        )],
    });

    assert_eq!(outcome, FileMutationBatchOutcome::Committed);
    assert_eq!(std::fs::read(&path)?, b"after");
    assert_eq!(
        std::fs::metadata(path)?.permissions().mode() & 0o7777,
        0o4751
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn replaced_created_directory_is_not_removed() -> io::Result<()> {
    let temp = TempDir::new()?;
    let directory = temp.path().join("new");
    let target = directory.join("file.txt");
    let outcome = mutate_batch_with_hook(
        FileMutationBatch {
            mutations: vec![write(&target, FilePreimage::Missing, b"batch")],
        },
        |checkpoint| match checkpoint {
            Checkpoint::AfterMutation(0) => {
                std::fs::remove_file(&target)?;
                std::fs::remove_dir(&directory)?;
                std::fs::create_dir(&directory)?;
                std::fs::write(directory.join("external.txt"), b"external")?;
                Err(io::Error::other("injected failure"))
            }
            Checkpoint::AfterPreflight
            | Checkpoint::BeforeWriteMutation(_)
            | Checkpoint::AfterWriteTruncate(_)
            | Checkpoint::BeforeWritePublish(_)
            | Checkpoint::AfterWritePublish(_)
            | Checkpoint::BeforeRemove(_)
            | Checkpoint::BeforeRemoveRename(_)
            | Checkpoint::AfterMutation(_)
            | Checkpoint::BeforeRollback(_)
            | Checkpoint::BeforeRollbackRemoveRename(_)
            | Checkpoint::BeforeRollbackRestoreRename(_)
            | Checkpoint::BeforeCreatedDirectoryRename(_) => Ok(()),
        },
    );

    assert!(matches!(
        outcome,
        FileMutationBatchOutcome::Indeterminate { .. }
    ));
    assert_eq!(std::fs::read(directory.join("external.txt"))?, b"external");
    Ok(())
}

#[test]
fn replacement_after_created_directory_check_is_not_removed() -> io::Result<()> {
    let temp = TempDir::new()?;
    let directory = temp.path().join("new");
    let moved_directory = temp.path().join("moved-new");
    let target = directory.join("file.txt");

    let outcome = mutate_batch_with_hook(
        FileMutationBatch {
            mutations: vec![write(&target, FilePreimage::Missing, b"batch")],
        },
        |checkpoint| match checkpoint {
            Checkpoint::AfterMutation(0) => Err(io::Error::other("injected failure")),
            Checkpoint::BeforeCreatedDirectoryRename(0) => {
                std::fs::rename(&directory, &moved_directory)?;
                std::fs::create_dir(&directory)?;
                std::fs::write(directory.join("external.txt"), b"external")
            }
            Checkpoint::AfterPreflight
            | Checkpoint::BeforeWriteMutation(_)
            | Checkpoint::AfterWriteTruncate(_)
            | Checkpoint::BeforeWritePublish(_)
            | Checkpoint::AfterWritePublish(_)
            | Checkpoint::BeforeRemove(_)
            | Checkpoint::BeforeRemoveRename(_)
            | Checkpoint::AfterMutation(_)
            | Checkpoint::BeforeRollback(_)
            | Checkpoint::BeforeRollbackRemoveRename(_)
            | Checkpoint::BeforeRollbackRestoreRename(_)
            | Checkpoint::BeforeCreatedDirectoryRename(_) => Ok(()),
        },
    );

    assert!(matches!(
        outcome,
        FileMutationBatchOutcome::Indeterminate { .. }
    ));
    assert_eq!(std::fs::read(directory.join("external.txt"))?, b"external");
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlink_swap_before_commit_does_not_modify_link_target() -> io::Result<()> {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new()?;
    let path = temp.path().join("file.txt");
    let external = temp.path().join("external.txt");
    std::fs::write(&path, b"before")?;
    std::fs::write(&external, b"external")?;
    let outcome = mutate_batch_with_hook(
        FileMutationBatch {
            mutations: vec![write(
                &path,
                FilePreimage::Exact(b"before".to_vec()),
                b"after",
            )],
        },
        |checkpoint| match checkpoint {
            Checkpoint::AfterPreflight => {
                std::fs::remove_file(&path)?;
                symlink(&external, &path)
            }
            Checkpoint::BeforeWriteMutation(_)
            | Checkpoint::AfterWriteTruncate(_)
            | Checkpoint::BeforeWritePublish(_)
            | Checkpoint::AfterWritePublish(_)
            | Checkpoint::BeforeRemove(_)
            | Checkpoint::BeforeRemoveRename(_)
            | Checkpoint::AfterMutation(_)
            | Checkpoint::BeforeRollback(_)
            | Checkpoint::BeforeRollbackRemoveRename(_)
            | Checkpoint::BeforeRollbackRestoreRename(_)
            | Checkpoint::BeforeCreatedDirectoryRename(_) => Ok(()),
        },
    );

    assert!(matches!(
        outcome,
        FileMutationBatchOutcome::RolledBack { .. }
    ));
    assert_eq!(std::fs::read(external)?, b"external");
    Ok(())
}

#[cfg(unix)]
#[test]
fn rollback_does_not_follow_a_swapped_parent_symlink() -> io::Result<()> {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new()?;
    let parent = temp.path().join("parent");
    let moved_parent = temp.path().join("moved-parent");
    let external = temp.path().join("external");
    std::fs::create_dir(&parent)?;
    std::fs::create_dir(&external)?;
    let path = parent.join("file.txt");
    let moved_path = moved_parent.join("file.txt");
    let external_path = external.join("file.txt");
    std::fs::write(&path, b"before")?;

    let outcome = mutate_batch_with_hook(
        FileMutationBatch {
            mutations: vec![write(
                &path,
                FilePreimage::Exact(b"before".to_vec()),
                b"after",
            )],
        },
        |checkpoint| match checkpoint {
            Checkpoint::AfterMutation(0) => {
                std::fs::rename(&parent, &moved_parent)?;
                std::fs::hard_link(&moved_path, &external_path)?;
                symlink(&external, &parent)?;
                Err(io::Error::other("injected failure"))
            }
            Checkpoint::AfterPreflight
            | Checkpoint::BeforeWriteMutation(_)
            | Checkpoint::AfterWriteTruncate(_)
            | Checkpoint::BeforeWritePublish(_)
            | Checkpoint::AfterWritePublish(_)
            | Checkpoint::BeforeRemove(_)
            | Checkpoint::BeforeRemoveRename(_)
            | Checkpoint::AfterMutation(_)
            | Checkpoint::BeforeRollback(_)
            | Checkpoint::BeforeRollbackRemoveRename(_)
            | Checkpoint::BeforeRollbackRestoreRename(_)
            | Checkpoint::BeforeCreatedDirectoryRename(_) => Ok(()),
        },
    );

    assert!(matches!(
        outcome,
        FileMutationBatchOutcome::Indeterminate {
            possibly_mutated_paths,
            ..
        } if possibly_mutated_paths == vec![path_uri(&path)]
    ));
    assert_eq!(std::fs::read(external_path)?, b"after");
    Ok(())
}

#[cfg(unix)]
#[test]
fn rollback_restores_unix_permissions() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new()?;
    let path = temp.path().join("executable");
    std::fs::write(&path, b"before")?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o751))?;
    let outcome = mutate_batch_with_hook(
        FileMutationBatch {
            mutations: vec![write(
                &path,
                FilePreimage::Exact(b"before".to_vec()),
                b"after",
            )],
        },
        |checkpoint| match checkpoint {
            Checkpoint::AfterMutation(0) => Err(io::Error::other("injected failure")),
            Checkpoint::AfterPreflight
            | Checkpoint::BeforeWriteMutation(_)
            | Checkpoint::AfterWriteTruncate(_)
            | Checkpoint::BeforeWritePublish(_)
            | Checkpoint::AfterWritePublish(_)
            | Checkpoint::BeforeRemove(_)
            | Checkpoint::BeforeRemoveRename(_)
            | Checkpoint::AfterMutation(_)
            | Checkpoint::BeforeRollback(_)
            | Checkpoint::BeforeRollbackRemoveRename(_)
            | Checkpoint::BeforeRollbackRestoreRename(_)
            | Checkpoint::BeforeCreatedDirectoryRename(_) => Ok(()),
        },
    );

    assert!(matches!(
        outcome,
        FileMutationBatchOutcome::RolledBack { .. }
    ));
    assert_eq!(std::fs::read(&path)?, b"before");
    assert_eq!(
        std::fs::metadata(path)?.permissions().mode() & 0o7777,
        0o751
    );
    Ok(())
}
