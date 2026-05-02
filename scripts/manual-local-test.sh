#!/bin/zsh
set -euo pipefail

SCRIPT_DIR=${0:A:h}
REPO_ROOT=${SCRIPT_DIR:h}

ROOT=${ROOT:-/tmp/pathsync-manual}
SIZE_MB=${SIZE_MB:-250}
RUN_FAILURE_TEST=${RUN_FAILURE_TEST:-1}
BIN=${PATHSYNC_BIN:-$REPO_ROOT/target/release/pathsync}

SOURCE="$ROOT/source"
TARGET_A="$ROOT/target-a"
TARGET_B="$ROOT/target-b"
CONFIG="$ROOT/config.toml"
FAIL_CONFIG="$ROOT/failure-config.toml"

log() {
  printf '\n==> %s\n' "$1"
}

require_bin() {
  if [[ -n "${PATHSYNC_BIN:-}" ]]; then
    if [[ ! -x "$BIN" ]]; then
      printf 'PATHSYNC_BIN is not executable: %s\n' "$BIN" >&2
      exit 1
    fi
    return 0
  fi

  log "building release binary"
  (cd "$REPO_ROOT" && cargo build --release)
}

make_data() {
  log "creating local sample tree in $ROOT"
  rm -rf "$ROOT"
  mkdir -p "$SOURCE" "$TARGET_A" "$TARGET_B"

  printf 'small file used to exercise the non-large-file path\n' > "$SOURCE/small-note.txt"

  log "creating large-a.bin (${SIZE_MB} MB)"
  dd if=/dev/urandom of="$SOURCE/large-a.bin" bs=1m count="$SIZE_MB" status=progress

  log "creating large-b.bin (${SIZE_MB} MB)"
  dd if=/dev/urandom of="$SOURCE/large-b.bin" bs=1m count="$SIZE_MB" status=progress
}

write_config() {
  cat > "$CONFIG" <<EOF
default_job = "local"
parallel = 4

[jobs.local]
enabled = true
source = "$SOURCE"
targets = ["$TARGET_A", "$TARGET_B"]
extensions = ["txt", "bin"]
compare = { mode = "size_mtime" }
transfer = { mode = "adaptive", large_file_threshold_mb = 1, large_file_slots = 2, max_large_per_target = 2 }
layout = { kind = "template", value = "pathsync-sample/{filename}" }
EOF

  log "wrote config: $CONFIG"
}

run_copy() {
  log "running forced multi-target copy"
  "$BIN" --config "$CONFIG" --force
}

verify_outputs() {
  log "verifying target files with cmp"
  for target in "$TARGET_A/pathsync-sample" "$TARGET_B/pathsync-sample"; do
    cmp -s "$SOURCE/small-note.txt" "$target/small-note.txt"
    cmp -s "$SOURCE/large-a.bin" "$target/large-a.bin"
    cmp -s "$SOURCE/large-b.bin" "$target/large-b.bin"
    printf 'verified %s\n' "$target"
  done
}

run_skip_check() {
  log "running again without --force to exercise no-op planning"
  "$BIN" --config "$CONFIG"
}

run_failure_check() {
  if [[ "$RUN_FAILURE_TEST" != "1" ]]; then
    return 0
  fi

  local open_target="$ROOT/open-target"
  local blocked_target="$ROOT/blocked-target"
  mkdir -p "$open_target" "$blocked_target"
  chmod 0555 "$blocked_target"

  cat > "$FAIL_CONFIG" <<EOF
default_job = "failure"
parallel = 2

[jobs.failure]
enabled = true
source = "$SOURCE"
targets = ["$open_target", "$blocked_target"]
extensions = ["txt"]
compare = { mode = "size_mtime" }
transfer = { mode = "adaptive", large_file_threshold_mb = 1, large_file_slots = 1, max_large_per_target = 1 }
layout = { kind = "template", value = "pathsync-failure/{filename}" }
EOF

  log "running expected failure check against read-only local target"
  set +e
  "$BIN" --config "$FAIL_CONFIG" --force
  local copy_status=$?
  set -e
  chmod 0755 "$blocked_target"

  if [[ "$copy_status" == "0" ]]; then
    printf 'expected failure did not occur\n' >&2
    exit 1
  fi

  printf 'failure path produced non-zero exit as expected: %s\n' "$copy_status"
}

main() {
  require_bin
  make_data
  write_config
  run_copy
  verify_outputs
  run_skip_check
  run_failure_check

  log "manual test complete"
  printf 'source:   %s\n' "$SOURCE"
  printf 'target A: %s\n' "$TARGET_A"
  printf 'target B: %s\n' "$TARGET_B"
  printf 'config:   %s\n' "$CONFIG"
}

main "$@"
