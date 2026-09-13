# Set HOME if not defined
HOME := env("HOME", "/Users/" + env("USER", ""))

# Build the project
build:
    cargo build --release

# Install the binary to ~/.local/bin/
install: build
    mkdir -p {{HOME}}/.local/bin
    cp target/release/pathsync {{HOME}}/.local/bin/

# Clean build artifacts
clean:
    cargo clean

# Run format, clippy, and tests
check:
    cargo fmt --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test

# Default recipe
default: build
