set shell := ["bash", "-euo", "pipefail", "-c"]

patch:
  cargo release patch --no-publish --execute

publish:
  cargo publish

ci:
  cargo fmt --all --check
  cargo clippy --all-targets --all-features --locked -- -D warnings
  cargo doc --no-deps --all-features --locked
  cargo package --no-verify --allow-dirty --locked
  cargo check --all-features --benches --locked
  cargo nextest run --locked
  cargo nextest run --no-default-features --locked
  cargo nextest run --features serde --locked
  cargo nextest run --features ssh --locked
  cargo nextest run --features cyrus-sasl --locked
  cargo nextest run --all-features --locked
  cargo test --doc --all-features --locked

