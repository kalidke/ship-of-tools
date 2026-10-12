#!/usr/bin/env python3
"""Cargo's test runner in CI: test-binary-bound.py SECONDS BINARY [ARG...] runs one test binary with its arguments,
passing its output through unchanged. A binary still running after SECONDS is killed, and the runner names the tests
libtest reported running for over 60 seconds that it never saw end, then exits 124; cargo's --no-fail-fast goes on to
the next binary. Otherwise it ends as the binary ended: with its exit code, or, for a binary a signal ended, by that
signal, after naming it."""
import os, re, signal, subprocess, sys, threading

bound, command = float(sys.argv[1]), sys.argv[2:]
warned, ended = [], set()
slow = re.compile(r"test (\S+) has been running for over 60 seconds")
done = re.compile(r"test (\S+) \.\.\. ")


def relay(source, sink):
    for raw in iter(source.readline, b""):
        sink.buffer.write(raw)
        sink.flush()
        line = raw.decode("utf-8", "replace")
        warned.extend(slow.findall(line))
        ended.update(done.findall(line))


# The relay is the one writer a test binary's output passes through: a stdout or stderr left non-blocking (the hosted
# macOS step's) would end it at the first full pipe, and the binary would block until its bound.
if os.name == "posix":
    for fd in (1, 2):
        os.set_blocking(fd, True)
child = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
relays = [threading.Thread(target=relay, args=pair, daemon=True) for pair in ((child.stdout, sys.stdout), (child.stderr, sys.stderr))]
for thread in relays:
    thread.start()
try:
    code = child.wait(timeout=bound)
except subprocess.TimeoutExpired:
    child.kill()
    child.wait()
    running = [name for name in dict.fromkeys(warned) if name not in ended] or ["none reported by libtest"]
    print(f"test-binary-bound: {command[0]} did not end within {bound:g} s and was killed; still running: "
          + ", ".join(running), flush=True)
    sys.exit(124)
for thread in relays:
    thread.join(timeout=5)
if code < 0:
    # A signal ended the binary: name it, then end this runner by it, so cargo reports the signal, not an exit code of
    # 128 + N. Python ignores SIGPIPE and handles SIGINT itself, so the signal's default action comes back first;
    # SIGKILL's cannot be changed.
    print(f"test-binary-bound: {command[0]} was ended by signal {-code} ({signal.strsignal(-code)})", file=sys.stderr, flush=True)
    if -code != signal.SIGKILL:
        signal.signal(-code, signal.SIG_DFL)
    os.kill(os.getpid(), -code)
sys.exit(code)
