.PHONY: build test fmt lint check run clean release install

# Build debug binary
build:
	cargo build

# Run all tests
test:
	cargo test

# Check formatting (non-destructive)
fmt-check:
	cargo fmt --check

# Auto-format
fmt:
	cargo fmt

# Lint
lint:
	cargo clippy -- -D warnings

# fmt + lint + test in one shot (what CI runs)
check: fmt-check lint test

# Run the daemon in foreground (dev mode)
run:
	cargo run -- serve

# Build release binary
release:
	cargo build --release

# Install binary to ~/.cargo/bin
install:
	cargo install --path .

# Remove build artifacts
clean:
	cargo clean
