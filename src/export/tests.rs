#![allow(clippy::unwrap_used)]

use super::shared::map_repo_path_to_fs;
use super::*;
use std::future::Future;

#[test]
fn map_repo_path_to_fs_normalizes_and_strips_prefix() {
    let root = Path::new("root");

    let out = map_repo_path_to_fs(root, None, "//trunk\\\\sub//./", true).unwrap();
    assert_eq!(out, root.join("trunk").join("sub"));

    let out = map_repo_path_to_fs(root, Some("trunk/"), "/trunk//./sub/file.txt", false).unwrap();
    assert_eq!(out, root.join("sub").join("file.txt"));
}

#[test]
fn fs_editor_rejects_parent_dir_paths() {
    let editor = FsEditor::new("tmp");
    assert!(editor.repo_path_to_fs("../x", false).is_err());
    assert!(editor.repo_path_to_fs("a/../x", false).is_err());
}

#[test]
fn tokio_fs_editor_rejects_parent_dir_paths() {
    let editor = TokioFsEditor::new("tmp");
    assert!(editor.repo_path_to_fs("../x", false).is_err());
    assert!(editor.repo_path_to_fs("a/../x", false).is_err());
}

#[cfg(unix)]
#[test]
fn fs_editor_rejects_symlink_export_root() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = temp.path().join("root-link");

    symlink(outside.path(), &root).unwrap();

    let mut editor = FsEditor::new(root);
    let err = editor
        .on_event(EditorEvent::OpenRoot {
            rev: None,
            token: "d0".to_string(),
        })
        .unwrap_err();
    assert!(matches!(err, SvnError::InvalidPath(_)));
}

#[cfg(unix)]
#[test]
fn tokio_fs_editor_rejects_symlink_export_root() {
    use std::os::unix::fs::symlink;

    run_async(async {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = temp.path().join("root-link");

        symlink(outside.path(), &root).unwrap();

        let mut editor = TokioFsEditor::new(root);
        let err = editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".to_string(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, SvnError::InvalidPath(_)));
    });
}

#[cfg(unix)]
#[test]
fn fs_editor_rejects_symlink_parent_dir() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let outside = tempfile::tempdir().unwrap();

    symlink(outside.path(), root.join("trunk")).unwrap();

    let mut editor = FsEditor::new(root);
    editor
        .on_event(EditorEvent::OpenRoot {
            rev: None,
            token: "d0".to_string(),
        })
        .unwrap();

    let err = editor
        .on_event(EditorEvent::AddFile {
            path: "trunk/hello.txt".to_string(),
            dir_token: "d0".to_string(),
            file_token: "f1".to_string(),
            copy_from: None,
        })
        .unwrap_err();
    assert!(matches!(err, SvnError::InvalidPath(_)));
}

#[cfg(unix)]
#[test]
fn tokio_fs_editor_rejects_symlink_parent_dir() {
    use std::os::unix::fs::symlink;

    run_async(async {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let outside = tempfile::tempdir().unwrap();

        symlink(outside.path(), root.join("trunk")).unwrap();

        let mut editor = TokioFsEditor::new(root);
        editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".to_string(),
            })
            .await
            .unwrap();

        let err = editor
            .on_event(EditorEvent::AddFile {
                path: "trunk/hello.txt".to_string(),
                dir_token: "d0".to_string(),
                file_token: "f1".to_string(),
                copy_from: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, SvnError::InvalidPath(_)));
    });
}

#[cfg(unix)]
#[test]
fn fs_editor_delete_entry_removes_symlink_without_following_target() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let outside = tempfile::tempdir().unwrap();
    let sentinel = outside.path().join("sentinel.txt");
    std::fs::write(&sentinel, b"sentinel").unwrap();

    symlink(outside.path(), root.join("trunk")).unwrap();

    let mut editor = FsEditor::new(root.clone());
    editor
        .on_event(EditorEvent::OpenRoot {
            rev: None,
            token: "d0".to_string(),
        })
        .unwrap();

    editor
        .on_event(EditorEvent::DeleteEntry {
            path: "trunk".to_string(),
            rev: 1,
            dir_token: "d0".to_string(),
        })
        .unwrap();

    assert!(!root.join("trunk").exists());
    assert!(sentinel.exists());
}

#[cfg(unix)]
#[test]
fn tokio_fs_editor_delete_entry_removes_symlink_without_following_target() {
    use std::os::unix::fs::symlink;

    run_async(async {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let outside = tempfile::tempdir().unwrap();
        let sentinel = outside.path().join("sentinel.txt");
        std::fs::write(&sentinel, b"sentinel").unwrap();

        symlink(outside.path(), root.join("trunk")).unwrap();

        let mut editor = TokioFsEditor::new(root.clone());
        editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".to_string(),
            })
            .await
            .unwrap();

        editor
            .on_event(EditorEvent::DeleteEntry {
                path: "trunk".to_string(),
                rev: 1,
                dir_token: "d0".to_string(),
            })
            .await
            .unwrap();

        assert!(!root.join("trunk").exists());
        assert!(sentinel.exists());
    });
}

#[cfg(windows)]
fn try_create_junction(link: &Path, target: &Path) -> bool {
    use std::process::Command;

    let cmd = format!("mklink /J \"{}\" \"{}\"", link.display(), target.display());
    let Ok(out) = Command::new("cmd").args(["/C", &cmd]).output() else {
        return false;
    };
    out.status.success()
}

#[cfg(windows)]
#[test]
fn fs_editor_rejects_junction_parent_dir() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let outside = tempfile::tempdir().unwrap();

    if !try_create_junction(&root.join("trunk"), outside.path()) {
        return;
    }

    let mut editor = FsEditor::new(root);
    editor
        .on_event(EditorEvent::OpenRoot {
            rev: None,
            token: "d0".to_string(),
        })
        .unwrap();

    let err = editor
        .on_event(EditorEvent::AddFile {
            path: "trunk/hello.txt".to_string(),
            dir_token: "d0".to_string(),
            file_token: "f1".to_string(),
            copy_from: None,
        })
        .unwrap_err();
    assert!(matches!(err, SvnError::InvalidPath(_)));
}

#[cfg(windows)]
#[test]
fn tokio_fs_editor_rejects_junction_parent_dir() {
    run_async(async {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let outside = tempfile::tempdir().unwrap();

        if !try_create_junction(&root.join("trunk"), outside.path()) {
            return;
        }

        let mut editor = TokioFsEditor::new(root);
        editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".to_string(),
            })
            .await
            .unwrap();

        let err = editor
            .on_event(EditorEvent::AddFile {
                path: "trunk/hello.txt".to_string(),
                dir_token: "d0".to_string(),
                file_token: "f1".to_string(),
                copy_from: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, SvnError::InvalidPath(_)));
    });
}

#[cfg(windows)]
#[test]
fn fs_editor_delete_entry_removes_junction_without_following_target() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let outside = tempfile::tempdir().unwrap();
    let sentinel = outside.path().join("sentinel.txt");
    std::fs::write(&sentinel, b"sentinel").unwrap();

    if !try_create_junction(&root.join("trunk"), outside.path()) {
        return;
    }

    let mut editor = FsEditor::new(root.clone());
    editor
        .on_event(EditorEvent::OpenRoot {
            rev: None,
            token: "d0".to_string(),
        })
        .unwrap();

    editor
        .on_event(EditorEvent::DeleteEntry {
            path: "trunk".to_string(),
            rev: 1,
            dir_token: "d0".to_string(),
        })
        .unwrap();

    assert!(!root.join("trunk").exists());
    assert!(sentinel.exists());
}

#[cfg(windows)]
#[test]
fn tokio_fs_editor_delete_entry_removes_junction_without_following_target() {
    run_async(async {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let outside = tempfile::tempdir().unwrap();
        let sentinel = outside.path().join("sentinel.txt");
        std::fs::write(&sentinel, b"sentinel").unwrap();

        if !try_create_junction(&root.join("trunk"), outside.path()) {
            return;
        }

        let mut editor = TokioFsEditor::new(root.clone());
        editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".to_string(),
            })
            .await
            .unwrap();

        editor
            .on_event(EditorEvent::DeleteEntry {
                path: "trunk".to_string(),
                rev: 1,
                dir_token: "d0".to_string(),
            })
            .await
            .unwrap();

        assert!(!root.join("trunk").exists());
        assert!(sentinel.exists());
    });
}

#[test]
fn fs_editor_rejects_unknown_directory_token() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();

    let mut editor = FsEditor::new(root);
    editor
        .on_event(EditorEvent::OpenRoot {
            rev: None,
            token: "d0".to_string(),
        })
        .unwrap();

    let err = editor
        .on_event(EditorEvent::AddFile {
            path: "hello.txt".to_string(),
            dir_token: "missing".to_string(),
            file_token: "f1".to_string(),
            copy_from: None,
        })
        .unwrap_err();
    assert!(matches!(err, SvnError::Protocol(_)));
}

#[test]
fn fs_editor_rejects_reused_file_token_before_close() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();

    let mut editor = FsEditor::new(root);
    editor
        .on_event(EditorEvent::OpenRoot {
            rev: None,
            token: "d0".to_string(),
        })
        .unwrap();
    editor
        .on_event(EditorEvent::AddFile {
            path: "one.txt".to_string(),
            dir_token: "d0".to_string(),
            file_token: "f1".to_string(),
            copy_from: None,
        })
        .unwrap();

    let err = editor
        .on_event(EditorEvent::AddFile {
            path: "two.txt".to_string(),
            dir_token: "d0".to_string(),
            file_token: "f1".to_string(),
            copy_from: None,
        })
        .unwrap_err();
    assert!(matches!(err, SvnError::Protocol(_)));
}

#[test]
fn tokio_fs_editor_rejects_unknown_directory_token() {
    run_async(async {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();

        let mut editor = TokioFsEditor::new(root);
        editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".to_string(),
            })
            .await
            .unwrap();

        let err = editor
            .on_event(EditorEvent::AddFile {
                path: "hello.txt".to_string(),
                dir_token: "missing".to_string(),
                file_token: "f1".to_string(),
                copy_from: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, SvnError::Protocol(_)));
    });
}

#[test]
fn tokio_fs_editor_rejects_reused_file_token_before_close() {
    run_async(async {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();

        let mut editor = TokioFsEditor::new(root);
        editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".to_string(),
            })
            .await
            .unwrap();
        editor
            .on_event(EditorEvent::AddFile {
                path: "one.txt".to_string(),
                dir_token: "d0".to_string(),
                file_token: "f1".to_string(),
                copy_from: None,
            })
            .await
            .unwrap();

        let err = editor
            .on_event(EditorEvent::AddFile {
                path: "two.txt".to_string(),
                dir_token: "d0".to_string(),
                file_token: "f1".to_string(),
                copy_from: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, SvnError::Protocol(_)));
    });
}

fn run_async<T>(f: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

#[test]
fn tokio_fs_editor_writes_fulltext_delta_to_disk() {
    run_async(async {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();

        let mut editor = TokioFsEditor::new(root.clone());
        editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".to_string(),
            })
            .await
            .unwrap();
        editor
            .on_event(EditorEvent::AddFile {
                path: "hello.txt".to_string(),
                dir_token: "d0".to_string(),
                file_token: "f1".to_string(),
                copy_from: None,
            })
            .await
            .unwrap();
        editor
            .on_event(EditorEvent::ApplyTextDelta {
                file_token: "f1".to_string(),
                base_checksum: None,
            })
            .await
            .unwrap();

        let delta = crate::svndiff::encode_fulltext_with_options(
            crate::svndiff::SvndiffVersion::V0,
            b"hello",
            0,
            1024,
        )
        .unwrap();
        editor
            .on_event(EditorEvent::TextDeltaChunk {
                file_token: "f1".to_string(),
                chunk: delta,
            })
            .await
            .unwrap();
        editor
            .on_event(EditorEvent::TextDeltaEnd {
                file_token: "f1".to_string(),
            })
            .await
            .unwrap();
        editor
            .on_event(EditorEvent::CloseFile {
                file_token: "f1".to_string(),
                text_checksum: None,
            })
            .await
            .unwrap();
        editor.on_event(EditorEvent::CloseEdit).await.unwrap();

        let written = tokio::fs::read(root.join("hello.txt")).await.unwrap();
        assert_eq!(written, b"hello");
    });
}

#[test]
fn fs_editor_copies_file_from_copyfrom_when_no_textdelta_is_sent() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();

    std::fs::write(root.join("src.txt"), b"hello").unwrap();

    let mut editor = FsEditor::new(root.clone());
    editor
        .on_event(EditorEvent::OpenRoot {
            rev: None,
            token: "d0".to_string(),
        })
        .unwrap();
    editor
        .on_event(EditorEvent::AddFile {
            path: "dst.txt".to_string(),
            dir_token: "d0".to_string(),
            file_token: "f1".to_string(),
            copy_from: Some(("src.txt".to_string(), 1)),
        })
        .unwrap();
    editor
        .on_event(EditorEvent::CloseFile {
            file_token: "f1".to_string(),
            text_checksum: Some("5d41402abc4b2a76b9719d911017c592".to_string()),
        })
        .unwrap();
    editor.on_event(EditorEvent::CloseEdit).unwrap();

    assert_eq!(std::fs::read(root.join("dst.txt")).unwrap(), b"hello");
}

#[test]
fn fs_editor_creates_empty_added_file_when_no_textdelta_is_sent() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();

    let mut editor = FsEditor::new(root.clone());
    editor
        .on_event(EditorEvent::OpenRoot {
            rev: None,
            token: "d0".to_string(),
        })
        .unwrap();
    editor
        .on_event(EditorEvent::AddFile {
            path: "empty.txt".to_string(),
            dir_token: "d0".to_string(),
            file_token: "f1".to_string(),
            copy_from: None,
        })
        .unwrap();
    editor
        .on_event(EditorEvent::CloseFile {
            file_token: "f1".to_string(),
            text_checksum: None,
        })
        .unwrap();
    editor.on_event(EditorEvent::CloseEdit).unwrap();

    assert_eq!(std::fs::read(root.join("empty.txt")).unwrap(), b"");
}

#[test]
fn fs_editor_rejects_no_textdelta_file_over_existing_dir() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();

    std::fs::create_dir(root.join("empty.txt")).unwrap();

    let mut editor = FsEditor::new(root);
    editor
        .on_event(EditorEvent::OpenRoot {
            rev: None,
            token: "d0".to_string(),
        })
        .unwrap();
    editor
        .on_event(EditorEvent::AddFile {
            path: "empty.txt".to_string(),
            dir_token: "d0".to_string(),
            file_token: "f1".to_string(),
            copy_from: None,
        })
        .unwrap();
    let err = editor
        .on_event(EditorEvent::CloseFile {
            file_token: "f1".to_string(),
            text_checksum: None,
        })
        .unwrap_err();
    assert!(matches!(err, SvnError::InvalidPath(_)));
}

#[test]
fn fs_editor_rejects_unverifiable_dir_copy() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let mut editor = FsEditor::new(root.clone());
    editor
        .on_event(EditorEvent::OpenRoot {
            rev: None,
            token: "d0".to_string(),
        })
        .unwrap();
    let err = editor
        .on_event(EditorEvent::AddDir {
            path: "destdir".to_string(),
            parent_token: "d0".to_string(),
            child_token: "d1".to_string(),
            copy_from: Some(("srcdir".to_string(), 1)),
        })
        .unwrap_err();
    assert!(matches!(err, SvnError::Protocol(_)));
    assert!(!root.join("destdir").exists());
}

#[test]
fn tokio_fs_editor_copies_file_from_copyfrom_when_no_textdelta_is_sent() {
    run_async(async {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();

        std::fs::write(root.join("src.txt"), b"hello").unwrap();

        let mut editor = TokioFsEditor::new(root.clone());
        editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".to_string(),
            })
            .await
            .unwrap();
        editor
            .on_event(EditorEvent::AddFile {
                path: "dst.txt".to_string(),
                dir_token: "d0".to_string(),
                file_token: "f1".to_string(),
                copy_from: Some(("src.txt".to_string(), 1)),
            })
            .await
            .unwrap();
        editor
            .on_event(EditorEvent::CloseFile {
                file_token: "f1".to_string(),
                text_checksum: Some("5d41402abc4b2a76b9719d911017c592".to_string()),
            })
            .await
            .unwrap();
        editor.on_event(EditorEvent::CloseEdit).await.unwrap();

        let written = tokio::fs::read(root.join("dst.txt")).await.unwrap();
        assert_eq!(written, b"hello");
    });
}

#[test]
fn tokio_fs_editor_creates_empty_added_file_when_no_textdelta_is_sent() {
    run_async(async {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();

        let mut editor = TokioFsEditor::new(root.clone());
        editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".to_string(),
            })
            .await
            .unwrap();
        editor
            .on_event(EditorEvent::AddFile {
                path: "empty.txt".to_string(),
                dir_token: "d0".to_string(),
                file_token: "f1".to_string(),
                copy_from: None,
            })
            .await
            .unwrap();
        editor
            .on_event(EditorEvent::CloseFile {
                file_token: "f1".to_string(),
                text_checksum: None,
            })
            .await
            .unwrap();
        editor.on_event(EditorEvent::CloseEdit).await.unwrap();

        let written = tokio::fs::read(root.join("empty.txt")).await.unwrap();
        assert_eq!(written, b"");
    });
}

#[test]
fn tokio_fs_editor_rejects_no_textdelta_file_over_existing_dir() {
    run_async(async {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();

        tokio::fs::create_dir(root.join("empty.txt")).await.unwrap();

        let mut editor = TokioFsEditor::new(root);
        editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".to_string(),
            })
            .await
            .unwrap();
        editor
            .on_event(EditorEvent::AddFile {
                path: "empty.txt".to_string(),
                dir_token: "d0".to_string(),
                file_token: "f1".to_string(),
                copy_from: None,
            })
            .await
            .unwrap();
        let err = editor
            .on_event(EditorEvent::CloseFile {
                file_token: "f1".to_string(),
                text_checksum: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, SvnError::InvalidPath(_)));
    });
}

#[test]
fn tokio_fs_editor_rejects_unverifiable_dir_copy() {
    run_async(async {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let mut editor = TokioFsEditor::new(root.clone());
        editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".to_string(),
            })
            .await
            .unwrap();
        let err = editor
            .on_event(EditorEvent::AddDir {
                path: "destdir".to_string(),
                parent_token: "d0".to_string(),
                child_token: "d1".to_string(),
                copy_from: Some(("srcdir".to_string(), 1)),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, SvnError::Protocol(_)));
        assert!(!root.join("destdir").exists());
    });
}

#[test]
fn fs_editor_rejects_wrong_copy_revision_content() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    std::fs::write(root.join("src.txt"), b"newer").unwrap();
    let mut editor = FsEditor::new(root.clone());
    editor
        .on_event(EditorEvent::OpenRoot {
            rev: None,
            token: "d0".into(),
        })
        .unwrap();
    editor
        .on_event(EditorEvent::AddFile {
            path: "dst.txt".into(),
            dir_token: "d0".into(),
            file_token: "f1".into(),
            copy_from: Some(("src.txt".into(), 1)),
        })
        .unwrap();
    let err = editor
        .on_event(EditorEvent::CloseFile {
            file_token: "f1".into(),
            text_checksum: Some("5d41402abc4b2a76b9719d911017c592".into()),
        })
        .unwrap_err();
    assert!(matches!(err, SvnError::Protocol(_)));
    assert!(!root.join("dst.txt").exists());
}

#[test]
fn tokio_fs_editor_rejects_wrong_copy_revision_content() {
    run_async(async {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        std::fs::write(root.join("src.txt"), b"newer").unwrap();
        let mut editor = TokioFsEditor::new(root.clone());
        editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".into(),
            })
            .await
            .unwrap();
        editor
            .on_event(EditorEvent::AddFile {
                path: "dst.txt".into(),
                dir_token: "d0".into(),
                file_token: "f1".into(),
                copy_from: Some(("src.txt".into(), 1)),
            })
            .await
            .unwrap();
        let err = editor
            .on_event(EditorEvent::CloseFile {
                file_token: "f1".into(),
                text_checksum: Some("5d41402abc4b2a76b9719d911017c592".into()),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, SvnError::Protocol(_)));
        assert!(!root.join("dst.txt").exists());
    });
}

#[test]
fn fs_editor_rejects_wrong_delta_checksum_before_replacing_file() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    std::fs::write(root.join("dst.txt"), b"original").unwrap();
    let mut editor = FsEditor::new(root.clone());
    editor
        .on_event(EditorEvent::OpenRoot {
            rev: None,
            token: "d0".into(),
        })
        .unwrap();
    editor
        .on_event(EditorEvent::OpenFile {
            path: "dst.txt".into(),
            dir_token: "d0".into(),
            file_token: "f1".into(),
            rev: 1,
        })
        .unwrap();
    editor
        .on_event(EditorEvent::ApplyTextDelta {
            file_token: "f1".into(),
            base_checksum: None,
        })
        .unwrap();
    editor
        .on_event(EditorEvent::TextDeltaEnd {
            file_token: "f1".into(),
        })
        .unwrap();
    let err = editor
        .on_event(EditorEvent::CloseFile {
            file_token: "f1".into(),
            text_checksum: Some("00000000000000000000000000000000".into()),
        })
        .unwrap_err();
    assert!(matches!(err, SvnError::Protocol(_)));
    assert_eq!(std::fs::read(root.join("dst.txt")).unwrap(), b"original");
}

#[test]
fn tokio_fs_editor_rejects_wrong_delta_checksum_before_replacing_file() {
    run_async(async {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        std::fs::write(root.join("dst.txt"), b"original").unwrap();
        let mut editor = TokioFsEditor::new(root.clone());
        editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".into(),
            })
            .await
            .unwrap();
        editor
            .on_event(EditorEvent::OpenFile {
                path: "dst.txt".into(),
                dir_token: "d0".into(),
                file_token: "f1".into(),
                rev: 1,
            })
            .await
            .unwrap();
        editor
            .on_event(EditorEvent::ApplyTextDelta {
                file_token: "f1".into(),
                base_checksum: None,
            })
            .await
            .unwrap();
        editor
            .on_event(EditorEvent::TextDeltaEnd {
                file_token: "f1".into(),
            })
            .await
            .unwrap();
        let err = editor
            .on_event(EditorEvent::CloseFile {
                file_token: "f1".into(),
                text_checksum: Some("00000000000000000000000000000000".into()),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, SvnError::Protocol(_)));
        assert_eq!(std::fs::read(root.join("dst.txt")).unwrap(), b"original");
    });
}
