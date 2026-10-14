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

use svn::{
    CommitBuilder, CommitOptions, EditorCommand, LockOptions, LogEntry, LogOptions, LogRevProps,
    NodeKind, PropertyList, RaSvnClient, RaSvnSession, SvnUrl, SvndiffMode, UnlockOptions,
};

fn run_async<T, F, Fut>(f: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = T> + Send + 'static,
{
    // Full debug interop futures can exceed libtest's default per-test stack.
    let handle = std::thread::Builder::new()
        .name("svn-interop-runtime".to_string())
        .stack_size(32 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(f())
        })
        .unwrap();

    match handle.join() {
        Ok(result) => result,
        Err(payload) => std::panic::resume_unwind(payload),
    }
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
    start_fixture_with_auth(false)
}

fn start_fixture_with_auth(require_auth: bool) -> SvnserveFixture {
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
    writeln!(
        svnserve_conf,
        "anon-access = {}",
        if require_auth { "none" } else { "read" }
    )
    .unwrap();
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

async fn read_file(session: &mut svn::RaSvnSession, path: &str, rev: u64) -> Vec<u8> {
    let mut out = VecWriter::new();
    session
        .get_file(path, rev, false, &mut out, 1024 * 1024)
        .await
        .unwrap();
    out.buf
}

fn seed_log_revprops(fixture: &SvnserveFixture) -> PropertyList {
    let props = PropertyList::from([
        ("svn:author".to_string(), b"alice".to_vec()),
        (
            "svn:date".to_string(),
            b"2025-01-02T03:04:05.000000Z".to_vec(),
        ),
        ("svn:log".to_string(), b"init".to_vec()),
        ("test:trace".to_string(), b"issue-30".to_vec()),
        ("test:opaque".to_string(), b"opaque\0\xffvalue".to_vec()),
    ]);
    let repo = fixture._tmp.path().join("root/repo");
    let repo_url = file_url(&repo);
    let value_file = fixture._tmp.path().join("revprop-value");
    for (name, value) in &props {
        std::fs::write(&value_file, value).unwrap();
        run_checked(
            "svn",
            &[
                "propset",
                "--revprop",
                "-r",
                "1",
                name,
                "--file",
                value_file.to_str().unwrap(),
                repo_url.as_str(),
                "--non-interactive",
                "--no-auth-cache",
            ],
            None,
        );
    }
    props
}

#[derive(Clone, Copy, Debug)]
enum LogApi {
    Convenience,
    Collected,
    Streaming,
    StreamingRetrying,
}

async fn collect_log(
    session: &mut RaSvnSession,
    api: LogApi,
    options: &LogOptions,
) -> Vec<LogEntry> {
    match api {
        LogApi::Convenience => session.log(1, 1).await.unwrap(),
        LogApi::Collected => session.log_with_options(options).await.unwrap(),
        LogApi::Streaming | LogApi::StreamingRetrying => {
            let mut entries = Vec::new();
            let on_entry = |entry| {
                entries.push(entry);
                Ok(())
            };
            let result = match api {
                LogApi::Streaming => session.log_each(options, on_entry).await,
                _ => session.log_each_retrying(options, on_entry).await,
            };
            result.unwrap_or_else(|err| panic!("{api:?} log failed: {err}"));
            entries
        }
    }
}

async fn assert_log_response(
    session: &mut RaSvnSession,
    entries: &[LogEntry],
    expected_props: &PropertyList,
    changed_paths: bool,
) {
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.rev, 1);
    assert_eq!(&entry.rev_props, expected_props);
    for (name, actual) in [
        ("svn:author", entry.author.as_deref()),
        ("svn:date", entry.date.as_deref()),
        ("svn:log", entry.message.as_deref()),
    ] {
        let expected = expected_props
            .get(name)
            .map(|value| std::str::from_utf8(value).unwrap());
        assert_eq!(actual, expected, "{name}");
    }
    if changed_paths {
        let mut paths: Vec<_> = entry
            .changed_paths
            .iter()
            .map(|path| (path.path.as_str(), path.action.as_str()))
            .collect();
        paths.sort_unstable();
        assert_eq!(paths, [("trunk", "A"), ("trunk/hello.txt", "A")]);
    } else {
        assert!(entry.changed_paths.is_empty());
    }
    assert!(!entry.has_children);
    assert!(!entry.invalid_revnum);
    assert!(!entry.subtractive_merge);
    // A successful log must consume its final response and leave the session usable.
    assert_eq!(session.get_latest_rev().await.unwrap(), 1);
}

async fn check_log_interop(require_auth: bool) {
    let mut fixture = start_fixture_with_auth(require_auth);
    let all_props = seed_log_revprops(&fixture);
    fixture.wait_ready().await;

    let url = SvnUrl::parse(&fixture.url()).unwrap();
    let (username, password) = if require_auth {
        (Some("alice".to_string()), Some("secret".to_string()))
    } else {
        (None, None)
    };
    // Retrying must not hide a malformed request that disconnects svnserve.
    let client = RaSvnClient::new(url, username, password)
        .with_reconnect_retries(0)
        .with_read_timeout(Duration::from_secs(5));
    let mut session = client.open_session().await.unwrap();
    let all_options = LogOptions::between(1, 1);
    for api in [
        LogApi::Convenience,
        LogApi::Collected,
        LogApi::Streaming,
        LogApi::StreamingRetrying,
    ] {
        let entries = collect_log(&mut session, api, &all_options).await;
        assert_log_response(&mut session, &entries, &all_props, true).await;
    }

    for names in [
        vec!["svn:author", "svn:date", "svn:log"],
        vec!["svn:author", "test:trace"],
        vec!["test:opaque"],
        vec![],
    ] {
        let expected_props: PropertyList = names
            .iter()
            .map(|name| ((*name).to_string(), all_props[*name].clone()))
            .collect();
        let options = LogOptions {
            changed_paths: false,
            revprops: LogRevProps::Custom(names.into_iter().map(str::to_string).collect()),
            ..LogOptions::between(1, 1)
        };
        for api in [
            LogApi::Collected,
            LogApi::Streaming,
            LogApi::StreamingRetrying,
        ] {
            let entries = collect_log(&mut session, api, &options).await;
            assert_log_response(&mut session, &entries, &expected_props, false).await;
        }
    }

    // Also exercise the client convenience wrapper against the real server.
    let entries = client.log(1, 1).await.unwrap();
    assert_log_response(&mut session, &entries, &all_props, true).await;
    assert_eq!(
        read_file(&mut session, "trunk/hello.txt", 1).await,
        b"hello\n"
    );
}

#[test]
fn interop_svnserve_log_revprops_anonymous() {
    if !interop_enabled() {
        return;
    }
    run_async(|| check_log_interop(false));
}

#[test]
fn interop_svnserve_log_revprops_authenticated() {
    if !interop_enabled() {
        return;
    }
    run_async(|| check_log_interop(true));
}

#[test]
fn interop_svnserve_readonly_smoke() {
    if !interop_enabled() {
        return;
    }

    run_async(|| async {
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

    run_async(|| async {
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

        let info = match session
            .commit(
                &CommitOptions::new("set svn:mime-type"),
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
                    EditorCommand::OpenFile {
                        path: "trunk/hello.txt".to_string(),
                        dir_token: "t".to_string(),
                        file_token: "f".to_string(),
                        rev: head,
                    },
                    EditorCommand::ChangeFileProp {
                        file_token: "f".to_string(),
                        name: "svn:mime-type".to_string(),
                        value: Some(b"text/plain".to_vec()),
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
        {
            Ok(info) => info,
            Err(err) => {
                let log = std::fs::read_to_string(&fixture.svnserve_stderr_log)
                    .unwrap_or_else(|_| "<failed to read svnserve log>".to_string());
                panic!("commit failed: {err}\nsvnserve log:\n{log}");
            }
        };
        assert_eq!(info.new_rev, head + 1);

        let props = session
            .proplist("trunk/hello.txt", Some(info.new_rev))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            props.get("svn:mime-type").unwrap().as_slice(),
            b"text/plain"
        );

        let builder = CommitBuilder::new()
            .with_base_rev(info.new_rev)
            .with_svndiff(SvndiffMode::Auto)
            .put_file("trunk/hello.txt", b"hello from svn-rs\n".to_vec());

        let info = session
            .commit_with_builder(&CommitOptions::new("edit file contents"), &builder)
            .await
            .unwrap();
        assert_eq!(info.new_rev, head + 2);

        let mut out = VecWriter::new();
        session
            .get_file(
                "trunk/hello.txt",
                info.new_rev,
                false,
                &mut out,
                1024 * 1024,
            )
            .await
            .unwrap();
        assert_eq!(out.buf, b"hello from svn-rs\n");
    });
}

#[test]
fn interop_svnserve_commit_builder_operations() {
    if !interop_enabled() {
        return;
    }

    run_async(|| async {
        let mut fixture = start_fixture();
        fixture.wait_ready().await;

        let url = SvnUrl::parse(&fixture.url()).unwrap();
        let client = RaSvnClient::new(url, Some("alice".to_string()), Some("secret".to_string()));
        let mut session = client.open_session().await.unwrap();

        let base = session.get_latest_rev().await.unwrap();
        let builder = CommitBuilder::new()
            .with_base_rev(base)
            .with_svndiff(SvndiffMode::Auto)
            .add_dir("trunk/newdir")
            .add_file("trunk/newdir/added.txt", b"added\n".to_vec())
            .replace_file("trunk/hello.txt", b"replaced\n".to_vec())
            .copy_file("trunk/hello.txt", "trunk/copied-hello.txt")
            .copy_dir("trunk", "branches/trunk-copy")
            .set_file_prop(
                "trunk/newdir/added.txt",
                "svn:mime-type",
                b"text/plain".to_vec(),
            );

        let info = session
            .commit_with_builder(&CommitOptions::new("builder add replace copy"), &builder)
            .await
            .unwrap();
        assert_eq!(info.new_rev, base + 1);

        assert_eq!(
            read_file(&mut session, "trunk/hello.txt", info.new_rev).await,
            b"replaced\n"
        );
        assert_eq!(
            read_file(&mut session, "trunk/newdir/added.txt", info.new_rev).await,
            b"added\n"
        );
        assert_eq!(
            read_file(&mut session, "trunk/copied-hello.txt", info.new_rev).await,
            b"hello\n"
        );
        assert_eq!(
            read_file(&mut session, "branches/trunk-copy/hello.txt", info.new_rev).await,
            b"hello\n"
        );

        let props = session
            .proplist("trunk/newdir/added.txt", Some(info.new_rev))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            props.get("svn:mime-type").unwrap().as_slice(),
            b"text/plain"
        );

        let builder = CommitBuilder::new()
            .with_base_rev(info.new_rev)
            .move_file("trunk/copied-hello.txt", "trunk/moved-copy.txt")
            .move_dir("branches/trunk-copy", "branches/trunk-moved");

        let info = session
            .commit_with_builder(&CommitOptions::new("builder move paths"), &builder)
            .await
            .unwrap();
        assert_eq!(info.new_rev, base + 2);

        assert_eq!(
            session
                .check_path("trunk/copied-hello.txt", Some(info.new_rev))
                .await
                .unwrap(),
            NodeKind::None
        );
        assert_eq!(
            session
                .check_path("branches/trunk-copy", Some(info.new_rev))
                .await
                .unwrap(),
            NodeKind::None
        );
        assert_eq!(
            read_file(&mut session, "trunk/moved-copy.txt", info.new_rev).await,
            b"hello\n"
        );
        assert_eq!(
            read_file(&mut session, "branches/trunk-moved/hello.txt", info.new_rev).await,
            b"hello\n"
        );
    });
}
