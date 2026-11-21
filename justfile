default:
    @just --list

# Format, lint, test, and audit. CI runs this recipe.
check: lint test audit

lint:
    cargo fmt --all --check
    cargo clippy --locked --workspace --all-targets -- -D warnings

test:
    cargo test --locked --workspace

audit:
    cargo deny check

fmt:
    cargo fmt --all

# Build the release binary: target/release/veux. It has no embedded
# helpers; see `release`.
build:
    cargo build --release --locked

# Build static helpers for remote hosts. libusb uses netlink, not udev,
# because a static musl binary cannot link the host libudev.
build-helpers:
    LIBUDEV_NO_PKG_CONFIG=1 cargo zigbuild --release --locked -p veux --target aarch64-unknown-linux-musl
    LIBUDEV_NO_PKG_CONFIG=1 cargo zigbuild --release --locked -p veux --target x86_64-unknown-linux-musl

# Build the release binary with the helpers embedded, for remote hosts.
release: build-helpers
    VEUX_EMBED_HELPER_AARCH64={{justfile_directory()}}/target/aarch64-unknown-linux-musl/release/veux \
    VEUX_EMBED_HELPER_X86_64={{justfile_directory()}}/target/x86_64-unknown-linux-musl/release/veux \
    cargo build --release --locked

# Run veux with the given arguments, for example `just run read living-room`.
# It builds with `release`, so the helpers for remote hosts are embedded.
run *args: release
    ./target/release/veux {{args}}
