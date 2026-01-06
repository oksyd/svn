# `svn` crate examples

Run an example with `cargo run --example <name>`.

## Common environment variables

- `SVN_URL=svn://host/repo` (required by most examples)
- `SVN_USERNAME` / `SVN_PASSWORD` (optional)
- `SVN_WRITE=1` (required for examples that perform real writes)

## Index

- `readonly`: read-only smoke (connect + HEAD + list + optional get-file)
- `log_retry`: resumable `log` streaming with auto-reconnect + dedup
- `capabilities`: print negotiated server capabilities
- `get_file`: stream `get-file` to a file (or discard), optionally with props/iprops
- `list`: `get-dir`, `list`, and `list_recursive`
- `props`: `proplist`, `propget`, `get-iprops`, `rev-prop(s)`
- `stat`: `check-path` + `stat`
- `dated_rev`: `get-dated-rev`
- `locations`: `get-locations`, `get-location-segments`, `get-deleted-rev`
- `mergeinfo`: `get-mergeinfo`
- `file_revs`: `get-file-revs` (+ `apply_textdelta_sync`), and `get_file_revs_with_contents`
- `reparent`: `reparent` a connected session
- `open_session_with_stream`: bring your own stream and call `open_session_with_stream`
- `update_events`: `update` editor drive + record/apply textdeltas
- `status_events`: `status` editor drive event counter (empty report)
- `diff_events`: `diff` editor drive event counter (empty report)
- `switch_events`: `switch` editor drive event counter (empty report)
- `replay_events`: `replay` event counter
- `replay_range_events`: `replay-range` event counter
- `export`: export a repository subtree to a local directory
- `pool`: `SessionPool` (limit concurrency + reuse sessions)
- `session_pools`: `SessionPools` (multi-host pool map + auto-reparent)

## Write examples (opt-in)

These examples perform real server writes and are disabled unless `SVN_WRITE=1`.

- `commit`: commit a file change using `CommitBuilder` / `CommitStreamBuilder`
- `locks`: `lock-many` / `unlock-many`
- `revprop`: `change-rev-prop*` (if supported by the server)

## Optional features

- `ssh`: `cargo run --example ssh --features ssh` (`svn+ssh://` via SSH tunnel)
- `sasl`: `cargo run --example sasl --features cyrus-sasl` (Cyrus SASL + security layer)

