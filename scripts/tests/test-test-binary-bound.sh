#!/usr/bin/env bash
# The CI test runner's own check: a test binary that hangs is ended at its bound with the hung test named, and one
# that ends keeps its output and exit code. RUNNER="" runs the stand-in binaries directly, as the step did before it.
set -u
here=$(cd "$(dirname "$0")" && pwd)
runner=${RUNNER-python3 $here/test-binary-bound.py 2}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
printf '#!/bin/sh\necho "test quick::ends ... ok"\necho "test slow::hangs has been running for over 60 seconds"\nexec sleep 600\n' > "$work/hangs"
printf '#!/bin/sh\necho "test quick::ends ... ok"\nexit 3\n' > "$work/ends"
chmod +x "$work/hangs" "$work/ends"
failed=0
start=$(date +%s)
out=$(timeout 20 $runner "$work/hangs" 2>&1); rc=$?
took=$(( $(date +%s) - start ))
if [ "$rc" = 124 ] && [ "$took" -lt 15 ] && grep -q "still running: slow::hangs$" <<< "$out"; then echo "ok: a hung binary ends at its bound, naming slow::hangs"
else echo "FAIL: a hung binary: exit $rc after ${took}s; output: $out"; failed=$((failed + 1)); fi
out=$($runner "$work/ends" 2>&1); rc=$?
if [ "$rc" = 3 ] && grep -q "quick::ends ... ok" <<< "$out"; then echo "ok: a binary that ends keeps its output and exit code"
else echo "FAIL: an ending binary: exit $rc; output: $out"; failed=$((failed + 1)); fi
printf '#!/bin/sh\ni=0; while [ $i -lt 20000 ]; do echo "line $i 0123456789abcdef0123456789abcdef"; i=$((i+1)); done\nexit 5\n' > "$work/floods"
chmod +x "$work/floods"
out=$(timeout 20 python3 -I -c '
import os, subprocess, sys, time
r, w = os.pipe()
os.set_blocking(w, False)
p = subprocess.Popen(sys.argv[1:], stdout=w, stderr=subprocess.DEVNULL)
os.close(w)
data = b""
while chunk := os.read(r, 4096):
    data += chunk
    time.sleep(0.002)
print(p.wait(), data.count(b"\n"))' $runner "$work/floods")
if [ "$out" = "5 20000" ]; then echo "ok: a flood through a non-blocking stdout keeps every line and its exit code"
else echo "FAIL: a flood through a non-blocking stdout: exit and lines $out, expected 5 20000"; failed=$((failed + 1)); fi
exit "$failed"
