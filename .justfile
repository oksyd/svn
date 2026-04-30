set shell := ["bash", "-euo", "pipefail", "-c"]

patch:
  cargo release patch --no-publish --execute

publish:
  cargo publish

ci:
  cargo fmt --all
  cargo clippy --all-targets --all-features  -- -D warnings
  cargo doc --no-deps --all-features
  cargo package --no-verify --allow-dirty
  cargo check --all-features --benches
  cargo nextest run
  cargo nextest run --no-default-features
  cargo nextest run --features serde
  cargo nextest run --features ssh
  cargo nextest run --features cyrus-sasl
  cargo nextest run --all-features
  cargo test --doc --all-features
