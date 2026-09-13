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

# Workspace hygiene: build artifacts, local scratch, stale git worktree metadata
cleanup: clean
    #!/usr/bin/env bash
    set -euo pipefail
    rm -rf .superpowers/brainstorm
    if [[ -d .superpowers ]] && [[ -z "$(find .superpowers -mindepth 1 -maxdepth 1 -print -quit 2>/dev/null)" ]]; then
      rmdir .superpowers
    fi
    find . -name .DS_Store -type f -delete 2>/dev/null || true
    git worktree prune
    echo "workspace cleaned"

# Run format, clippy, and tests
check:
    cargo fmt --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test

# Bump Cargo.toml version: patch | minor | major
# Requires cargo-edit (`cargo install cargo-edit`).
bump level:
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{level}}" in
      patch|minor|major) ;;
      *)
        echo "usage: just bump patch|minor|major" >&2
        exit 2
        ;;
    esac
    if ! cargo set-version --help >/dev/null 2>&1; then
      echo "cargo-set-version missing; installing cargo-edit..." >&2
      TMPDIR="${TMPDIR:-/tmp}" cargo install cargo-edit --locked
    fi
    cargo set-version --bump "{{level}}"
    version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
    echo "version -> ${version}"

# Bump, commit, tag, and push a release (working tree must be clean)
release level="patch":
    #!/usr/bin/env bash
    set -euo pipefail
    if ! git diff --quiet || ! git diff --cached --quiet; then
      echo "working tree dirty; commit or stash first" >&2
      exit 1
    fi
    just bump "{{level}}"
    version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
    git add Cargo.toml
    git commit -m "chore: release v${version}"
    git tag "v${version}"
    git push origin HEAD
    git push origin "v${version}"
    echo "released v${version}"

# Default recipe
default: build
