<div align="right">

<a href="README.md">🇺🇸 English</a> ·
<span style="color:#999;">🇨🇳 中文</span> &nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;|&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;
<a href="#table-of-contents">目录</a>

</div>

<h1 align="center"><code>svn-rs</code></h1>

<p align="center">基于 Rust 的异步 Subversion <code>svn://</code>（<code>ra_svn</code>）客户端 —— <code>libsvn_ra_svn</code> 的现代替代方案。</p>

<div align="center">
  <a href="https://crates.io/crates/svn">
    <img src="https://img.shields.io/crates/v/svn.svg" alt="crates.io version">
  </a>
  <a href="https://docs.rs/svn">
    <img src="https://img.shields.io/docsrs/svn?logo=rust" alt="docs.rs docs">
  </a>
  <a href="https://github.com/lvillis/svn-rs/actions">
    <img src="https://github.com/lvillis/svn-rs/actions/workflows/ci.yml/badge.svg" alt="CI status">
  </a>
  <a href="LICENSE">
    <img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="license">
  </a>
  <a href="rust-toolchain.toml">
    <img src="https://img.shields.io/badge/MSRV-1.92.0-informational" alt="MSRV 1.92.0">
  </a>
  <a href="https://crates.io/crates/svn">
    <img src="https://img.shields.io/crates/dr/svn?color=ba86eb" alt="downloads">
  </a>
</div>

---

<a id="table-of-contents"></a>

## 特性

- Async-first 的 `svn://`（`ra_svn`）客户端（不包含 working copy）。
- 高层 API：`RaSvnClient` / `RaSvnSession`。
- 结构化的服务端错误（`code/message/file/line`）并带命令上下文。
- 可选 `serde` feature：为公开数据类型提供 `Serialize`/`Deserialize`。
- 可选 `cyrus-sasl` feature：启用 Cyrus SASL 认证，并在协商成功时启用 SASL security
  layer（运行时需要系统提供 `libsasl2`）。

## 安装

添加依赖：

```bash
cargo add svn
```

或在 `Cargo.toml` 中：

```toml
[dependencies]
svn = "0.1"
```

你还需要一个异步运行时，以下示例使用 `tokio`：

```toml
[dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## 快速开始

`RaSvnClient` 是可复用的配置对象；用它创建已连接的 `RaSvnSession`（单条 TCP 连接）。

```rust,no_run
use std::time::Duration;
use svn::{RaSvnClient, SvnUrl};

#[tokio::main]
async fn main() -> svn::Result<()> {
    let url = SvnUrl::parse("svn://example.com/repo")?;

    let client = RaSvnClient::new(url, None, None)
        .with_connect_timeout(Duration::from_secs(10))
        .with_read_timeout(Duration::from_secs(30))
        .with_write_timeout(Duration::from_secs(30));

    let mut session = client.open_session().await?;
    let latest = session.get_latest_rev().await?;
    println!("HEAD = {latest}");

    Ok(())
}
```

## 配置

`RaSvnClient` 支持 builder 风格配置：

- `with_connect_timeout(Duration)`
- `with_read_timeout(Duration)`
- `with_write_timeout(Duration)`
- `with_ra_client(String)`（握手阶段发送给服务器）

生产环境通常建议复用同一个 `RaSvnSession`，避免每次操作都重复连接与握手。

## 认证

本库支持 `svnserve` 常见的 `svn://` 认证机制：

- `ANONYMOUS`
- `PLAIN`（用户名 + 密码）
- `CRAM-MD5`（用户名 + 密码）

创建 `RaSvnClient` 时传入 `username`/`password`。如果服务器要求不支持的机制，会返回 `SvnError::AuthUnavailable`。

如需启用 Cyrus SASL（以及可选的 SASL security layer），请开启 `cyrus-sasl` feature
（运行时需要系统已安装 `libsasl2`）：

```toml
svn = { version = "0.1", features = ["cyrus-sasl"] }
```

## 已支持的操作

本库面向 `ra_svn` 协议 v2，目前支持：

- 只读：`get-latest-rev`、`get-dated-rev`、`get-file`、`get-dir`、`log`、`list`、
  `check-path`、`stat`、`get-mergeinfo`、`get-deleted-rev`、`get-locations`、
  `get-location-segments`、`get-file-revs`、`rev-prop`、`rev-proplist`、`proplist`、
  `propget`、`get-iprops`、锁查询（`get-lock`、`get-locks`）。
- Report/editor 流：`update`、`switch`、`status`、`diff`、`replay`、`replay-range`。
- 写入：`change-rev-prop`、`change-rev-prop2`、`lock`/`unlock`（含 `*-many`）、以及由
  `EditorCommand` 驱动的低层 `commit` API。

完整 API 文档与更多示例请见：https://docs.rs/svn

## 错误处理

所有 API 返回 `svn::Result<T>`（即 `Result<T, SvnError>`）。服务端 `failure` 会返回
`SvnError::Server(ServerError)`，其中包含结构化错误链与命令上下文。

```rust,no_run
use svn::{RaSvnClient, SvnError, SvnUrl};

#[tokio::main]
async fn main() {
    let url = SvnUrl::parse("svn://example.com/repo").unwrap();
    let client = RaSvnClient::new(url, None, None);

    let err = client.get_latest_rev().await.unwrap_err();
    match err {
        SvnError::Server(server) => {
            eprintln!("server: {server}");
            for item in &server.chain {
                eprintln!(
                    "  code={} msg={:?} file={:?} line={:?}",
                    item.code, item.message, item.file, item.line
                );
            }
        }
        other => eprintln!("error: {other}"),
    }
}
```

## 日志

本库使用 `tracing` 输出 debug 日志。你可以在应用中引入 `tracing-subscriber` 并设置过滤：

```text
RUST_LOG=svn=debug
```

## 兼容性

- 协议：`ra_svn` v2（仅 `svn://`）。
- MSRV：Rust `1.92.0`（见 `Cargo.toml`）。
- `serde`：通过 `serde` feature 可选开启。
- Cyrus SASL：通过 `cyrus-sasl` feature 可选开启（运行时依赖 `libsasl2`）。

## 安全性

- `svn://` 是纯 TCP（不提供原生 TLS）。
- `PLAIN` 会在未加密的链路上发送凭据，除非你使用安全隧道（VPN / SSH 端口转发 / stunnel）或协商到 SASL security layer。
- 即使使用 `CRAM-MD5`，仓库数据流量仍然不会自动加密；仍需要隧道或 SASL security layer。

## 测试

单元测试 + 性质测试：

```bash
cargo test --all-features
```

对真实 `svnserve` 的互操作测试（需要 `svn`、`svnadmin`、`svnserve`）：

```bash
SVN_INTEROP=1 cargo test --all-features --test interop_svnserve -- --nocapture
```

## 限制

- 不支持 `svn+ssh://`。
- 不是 working copy 客户端（不提供 checkout / 本地工作副本更新）。
- 不提供原生 TLS（见 [安全性](#安全性)）。

## 许可证

本项目采用 MIT 许可证。见 `LICENSE`。
