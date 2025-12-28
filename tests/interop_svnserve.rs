//! Optional interoperability tests against a real `svnserve` instance.
//!
//! These tests are opt-in: set `SVN_INTEROP=1` and ensure `svnadmin`, `svnserve`,
//! and `svn` are available on `PATH`.

#![allow(clippy::expect_used)]
#![allow(clippy::panic)]
#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use svn::{CommitOptions, EditorCommand, LockOptions, RaSvnClient, SvnUrl, UnlockOptions};

fn run_async<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

fn interop_enabled() -> bool {
    matches!(
        std::env::var("SVN_INTEROP").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes") | Ok("YES")
    )
}

fn command_exists(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

fn run_checked(program: &str, args: &[&str], cwd: Option<&Path>) {
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(Stdio::null());
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let out = cmd.output().unwrap();
    if !out.status.success() {
        panic!(
            "{program} {:?} failed: {}\nstdout:\n{}\nstderr:\n{}",
            args,
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn file_url(path: &Path) -> String {
    let canonical = path.canonicalize().unwrap();
    let s = canonical.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

struct SvnserveFixture {
    _tmp: tempfile::TempDir,
    port: u16,
    svnserve: Child,
    svnserve_stderr_log: std::path::PathBuf,
}

impl Drop for SvnserveFixture {
    fn drop(&mut self) {
        let _ = self.svnserve.kill();
        let _ = self.svnserve.wait();
    }
}

impl SvnserveFixture {
    fn url(&self) -> String {
        format!("svn://127.0.0.1:{}/repo", self.port)
    }

    async fn wait_ready(&mut self) {
        for _ in 0..200 {
            if let Ok(Some(status)) = self.svnserve.try_wait() {
                let stderr = std::fs::read_to_string(&self.svnserve_stderr_log)
                    .unwrap_or_else(|_| "<failed to read svnserve stderr log>".to_string());
                panic!("svnserve exited early: {status}\nstderr:\n{stderr}");
            }
            if tokio::net::TcpStream::connect(("127.0.0.1", self.port))
                .await
                .is_ok()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        let stderr = std::fs::read_to_string(&self.svnserve_stderr_log)
            .unwrap_or_else(|_| "<failed to read svnserve stderr log>".to_string());
        panic!(
            "svnserve did not become ready on port {}\nstderr:\n{}",
            self.port, stderr
        );
    }
}

fn start_fixture() -> SvnserveFixture {
    if !interop_enabled() {
        panic!("SVN_INTEROP not enabled");
    }
    for bin in ["svnadmin", "svnserve", "svn"] {
        if !command_exists(bin) {
            panic!("{bin} is required for interop tests");
        }
    }

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let repo = root.join("repo");
    run_checked("svnadmin", &["create", repo.to_str().unwrap()], None);

    let conf = repo.join("conf");
    let mut svnserve_conf = std::fs::File::create(conf.join("svnserve.conf")).unwrap();
    writeln!(svnserve_conf, "[general]").unwrap();
    writeln!(svnserve_conf, "anon-access = read").unwrap();
    writeln!(svnserve_conf, "auth-access = write").unwrap();
    writeln!(svnserve_conf, "password-db = passwd").unwrap();
    writeln!(svnserve_conf, "realm = svn-rs-test").unwrap();

    let mut passwd = std::fs::File::create(conf.join("passwd")).unwrap();
    writeln!(passwd, "[users]").unwrap();
    writeln!(passwd, "alice = secret").unwrap();

    let import_dir = tmp.path().join("import");
    std::fs::create_dir_all(import_dir.join("trunk")).unwrap();
    std::fs::write(import_dir.join("trunk/hello.txt"), b"hello\n").unwrap();

    let repo_url = file_url(&repo);
    run_checked(
        "svn",
        &[
            "import",
            import_dir.to_str().unwrap(),
            repo_url.as_str(),
            "-m",
            "init",
            "--non-interactive",
        ],
        None,
    );

    let hook = repo.join("hooks").join("pre-revprop-change");
    std::fs::write(&hook, b"#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&hook).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&hook, perms).unwrap();
    }

    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let svnserve_stderr_log = tmp.path().join("svnserve.stderr.log");
    let log = std::fs::File::create(&svnserve_stderr_log).unwrap();
    let log_err = log.try_clone().unwrap();
    let child = Command::new("svnserve")
        .arg("-d")
        .arg("--foreground")
        .arg("-r")
        .arg(&root)
        .arg("--listen-host")
        .arg("127.0.0.1")
        .arg("--listen-port")
        .arg(port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .spawn()
        .unwrap();

    SvnserveFixture {
        _tmp: tmp,
        port,
        svnserve: child,
        svnserve_stderr_log,
    }
}

struct VecWriter {
    buf: Vec<u8>,
}

impl VecWriter {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }
}

impl tokio::io::AsyncWrite for VecWriter {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.buf.extend_from_slice(buf);
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[test]
fn interop_svnserve_readonly_smoke() {
    if !interop_enabled() {
        return;
    }

    run_async(async {
        let mut fixture = start_fixture();
        fixture.wait_ready().await;

        let url = SvnUrl::parse(&fixture.url()).unwrap();
        let client = RaSvnClient::new(url, None, None);
        let mut session = client.open_session().await.unwrap();

        let head = session.get_latest_rev().await.unwrap();
        assert!(head >= 1);

        let listing = session.list_dir("trunk", Some(head)).await.unwrap();
        assert!(listing.entries.iter().any(|e| e.name == "hello.txt"));

        let mut out = VecWriter::new();
        session
            .get_file("trunk/hello.txt", head, false, &mut out, 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(out.buf, b"hello\n");
    });
}

#[test]
fn interop_svnserve_write_lock_unlock_and_commit_smoke() {
    if !interop_enabled() {
        return;
    }

    run_async(async {
        let mut fixture = start_fixture();
        fixture.wait_ready().await;

        let url = SvnUrl::parse(&fixture.url()).unwrap();
        let client = RaSvnClient::new(url, Some("alice".to_string()), Some("secret".to_string()));
        let mut session = client.open_session().await.unwrap();

        let head = session.get_latest_rev().await.unwrap();
        assert!(head >= 1);

        let lock = session
            .lock("trunk/hello.txt", &LockOptions::new())
            .await
            .unwrap();
        assert_eq!(lock.path, "trunk/hello.txt");

        session
            .unlock(
                "trunk/hello.txt",
                &UnlockOptions::new().with_token(lock.token.clone()),
            )
            .await
            .unwrap();

        let info = session
            .commit(
                &CommitOptions::new("add empty file"),
                &[
                    EditorCommand::OpenRoot {
                        rev: Some(head),
                        token: "r".to_string(),
                    },
                    EditorCommand::OpenDir {
                        path: "trunk".to_string(),
                        parent_token: "r".to_string(),
                        child_token: "t".to_string(),
                        rev: head,
                    },
                    EditorCommand::AddFile {
                        path: "trunk/empty.txt".to_string(),
                        dir_token: "t".to_string(),
                        file_token: "f".to_string(),
                        copy_from: None,
                    },
                    EditorCommand::CloseFile {
                        file_token: "f".to_string(),
                        text_checksum: None,
                    },
                    EditorCommand::CloseDir {
                        dir_token: "t".to_string(),
                    },
                    EditorCommand::CloseDir {
                        dir_token: "r".to_string(),
                    },
                    EditorCommand::CloseEdit,
                ],
            )
            .await
            .unwrap();
        assert_eq!(info.new_rev, head + 1);

        let listing = session.list_dir("trunk", Some(info.new_rev)).await.unwrap();
        assert!(listing.entries.iter().any(|e| e.name == "empty.txt"));
    });
}
