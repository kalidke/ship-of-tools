#!/usr/bin/env bash
# Both Linux gate entries take this lock inside their test container, before setup or builds. Descriptor 9 stays
# open for the whole gate, including its children; never unlink the lock file, whose inode is the lock's identity.
gate_target_lock() {
    command -v flock > /dev/null || { echo "gate-target: flock is required to lock $CARGO_TARGET_DIR" >&2; return 2; }
    mkdir -p "$CARGO_TARGET_DIR" || return 2
    CARGO_TARGET_DIR=$(cd "$CARGO_TARGET_DIR" && pwd -P) || return 2
    export CARGO_TARGET_DIR
    exec 9>> "$CARGO_TARGET_DIR/gate.lock" || return 2
    flock -n 9 || { echo "gate-target: cannot lock target folder $CARGO_TARGET_DIR (another gate may be running)" >&2; return 2; }
}
