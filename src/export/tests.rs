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
            text_checksum: None,
        })
        .unwrap();
    editor.on_event(EditorEvent::CloseEdit).unwrap();

    assert_eq!(std::fs::read(root.join("dst.txt")).unwrap(), b"hello");
}

#[test]
fn fs_editor_copies_dir_from_copyfrom() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();

    std::fs::create_dir_all(root.join("srcdir/sub")).unwrap();
    std::fs::write(root.join("srcdir/sub/file.txt"), b"hello").unwrap();

    let mut editor = FsEditor::new(root.clone());
    editor
        .on_event(EditorEvent::OpenRoot {
            rev: None,
            token: "d0".to_string(),
        })
        .unwrap();
    editor
        .on_event(EditorEvent::AddDir {
            path: "destdir".to_string(),
            parent_token: "d0".to_string(),
            child_token: "d1".to_string(),
            copy_from: Some(("srcdir".to_string(), 1)),
        })
        .unwrap();
    editor
        .on_event(EditorEvent::CloseDir {
            dir_token: "d1".to_string(),
        })
        .unwrap();
    editor.on_event(EditorEvent::CloseEdit).unwrap();

    assert_eq!(
        std::fs::read(root.join("destdir/sub/file.txt")).unwrap(),
        b"hello"
    );
}

#[test]
fn fs_editor_dir_copyfrom_provides_base_for_identity_textdelta() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();

    std::fs::create_dir_all(root.join("srcdir/sub")).unwrap();
    std::fs::write(root.join("srcdir/sub/file.txt"), b"hello").unwrap();

    let mut editor = FsEditor::new(root.clone());
    editor
        .on_event(EditorEvent::OpenRoot {
            rev: None,
            token: "d0".to_string(),
        })
        .unwrap();
    editor
        .on_event(EditorEvent::AddDir {
            path: "destdir".to_string(),
            parent_token: "d0".to_string(),
            child_token: "d1".to_string(),
            copy_from: Some(("srcdir".to_string(), 1)),
        })
        .unwrap();
    editor
        .on_event(EditorEvent::OpenFile {
            path: "destdir/sub/file.txt".to_string(),
            dir_token: "d1".to_string(),
            file_token: "f1".to_string(),
            rev: 1,
        })
        .unwrap();
    editor
        .on_event(EditorEvent::ApplyTextDelta {
            file_token: "f1".to_string(),
            base_checksum: None,
        })
        .unwrap();
    editor
        .on_event(EditorEvent::TextDeltaEnd {
            file_token: "f1".to_string(),
        })
        .unwrap();
    editor
        .on_event(EditorEvent::CloseFile {
            file_token: "f1".to_string(),
            text_checksum: None,
        })
        .unwrap();
    editor
        .on_event(EditorEvent::CloseDir {
            dir_token: "d1".to_string(),
        })
        .unwrap();
    editor.on_event(EditorEvent::CloseEdit).unwrap();

    assert_eq!(
        std::fs::read(root.join("destdir/sub/file.txt")).unwrap(),
        b"hello"
    );
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
                text_checksum: None,
            })
            .await
            .unwrap();
        editor.on_event(EditorEvent::CloseEdit).await.unwrap();

        let written = tokio::fs::read(root.join("dst.txt")).await.unwrap();
        assert_eq!(written, b"hello");
    });
}

#[test]
fn tokio_fs_editor_copies_dir_from_copyfrom() {
    run_async(async {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();

        std::fs::create_dir_all(root.join("srcdir/sub")).unwrap();
        std::fs::write(root.join("srcdir/sub/file.txt"), b"hello").unwrap();

        let mut editor = TokioFsEditor::new(root.clone());
        editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".to_string(),
            })
            .await
            .unwrap();
        editor
            .on_event(EditorEvent::AddDir {
                path: "destdir".to_string(),
                parent_token: "d0".to_string(),
                child_token: "d1".to_string(),
                copy_from: Some(("srcdir".to_string(), 1)),
            })
            .await
            .unwrap();
        editor
            .on_event(EditorEvent::CloseDir {
                dir_token: "d1".to_string(),
            })
            .await
            .unwrap();
        editor.on_event(EditorEvent::CloseEdit).await.unwrap();

        let written = tokio::fs::read(root.join("destdir/sub/file.txt"))
            .await
            .unwrap();
        assert_eq!(written, b"hello");
    });
}

#[test]
fn tokio_fs_editor_dir_copyfrom_provides_base_for_identity_textdelta() {
    run_async(async {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();

        std::fs::create_dir_all(root.join("srcdir/sub")).unwrap();
        std::fs::write(root.join("srcdir/sub/file.txt"), b"hello").unwrap();

        let mut editor = TokioFsEditor::new(root.clone());
        editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".to_string(),
            })
            .await
            .unwrap();
        editor
            .on_event(EditorEvent::AddDir {
                path: "destdir".to_string(),
                parent_token: "d0".to_string(),
                child_token: "d1".to_string(),
                copy_from: Some(("srcdir".to_string(), 1)),
            })
            .await
            .unwrap();
        editor
            .on_event(EditorEvent::OpenFile {
                path: "destdir/sub/file.txt".to_string(),
                dir_token: "d1".to_string(),
                file_token: "f1".to_string(),
                rev: 1,
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
        editor
            .on_event(EditorEvent::CloseDir {
                dir_token: "d1".to_string(),
            })
            .await
            .unwrap();
        editor.on_event(EditorEvent::CloseEdit).await.unwrap();

        let written = tokio::fs::read(root.join("destdir/sub/file.txt"))
            .await
            .unwrap();
        assert_eq!(written, b"hello");
    });
}
