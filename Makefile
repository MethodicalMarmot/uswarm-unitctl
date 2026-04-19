RUST_VERSION = 1.94
PI_TARGET = aarch64-unknown-linux-gnu

.PHONY: build release release-pi test format lint schema setup

setup:
	@echo "==> Checking Rust toolchain..."
	@command -v rustup >/dev/null 2>&1 || { echo "==> Installing rustup..."; curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y; . "$$HOME/.cargo/env"; }
	@command -v cargo >/dev/null 2>&1 || { echo "cargo not found after rustup install. Ensure $$HOME/.cargo/bin is in PATH."; exit 1; }
	@echo "==> Installing Rust $(RUST_VERSION) toolchain..."
	rustup update stable
	@echo "==> Ensuring clippy and rustfmt are installed..."
	rustup component add clippy rustfmt
	@echo "==> Installing cargo dependencies..."
	@command -v cross >/dev/null 2>&1 && echo "cross already installed" || cargo install cross --git https://github.com/cross-rs/cross
	@command -v rtk >/dev/null 2>&1 && echo "rtk already installed" || cargo install --git https://github.com/rtk-ai/rtk
	@echo "==> Setup complete."

build:
	cargo build

release:
	cargo build --release --bin unitctl

release-pi:
	cross build --release --bin unitctl --target $(PI_TARGET)

test:
	cargo test --lib --bin "*"

test-int:
	cargo test --test "*"

format:
	cargo fmt

lint:
	cargo fmt --check
	cargo clippy -- -D warnings

schema:
	cargo run --bin generate-schema

