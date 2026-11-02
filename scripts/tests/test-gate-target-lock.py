#!/usr/bin/env python3
"""Exercise both real gate entries with recording setup tools, without builds or a user manager."""
import os
from pathlib import Path
import subprocess
import tempfile
import time


scripts = Path(__file__).resolve().parent
with tempfile.TemporaryDirectory(prefix="gate-target-lock-") as scratch:
    root = Path(scratch).resolve()
    checkout = root / "checkout"
    (checkout / "scripts").mkdir(parents=True)
    tools = root / "tools"
    tools.mkdir()
    # A gate pauses after it has acquired its lock. Closing its input releases the fixture naturally.
    pause = '#!/bin/sh\nprintf ready > "$GATE_READY"\nread -r release\nexit '
    (checkout / "scripts/setup-publish-guard.sh").write_text(pause + "23\n")
    manager = tools / "systemctl"
    manager.write_text(pause + "1\n")
    manager.chmod(0o755)
    (root / "home").mkdir()
    env = dict(os.environ, HOME=str(root / "home"), PATH=f"{tools}:/usr/bin:/bin")
    target = root / "target"
    target.mkdir()
    alias = root / "alias"
    alias.symlink_to(target, target_is_directory=True)
    entries = ["rc-gate.sh", "user-manager-tests.sh"]

    def command(entry):
        return ["/bin/bash", str(scripts / entry), str(checkout), str(root / "logs")]

    def attempt(entry, folder, ready):
        return subprocess.run(command(entry), env=dict(env, CARGO_TARGET_DIR=str(folder), GATE_READY=str(ready)),
                              input="release\n", text=True, capture_output=True, timeout=3)

    for entry in entries:
        ready = root / (entry + ".ready")
        with (root / "holder.log").open("w") as output:
            holder = subprocess.Popen(command(entry), env=dict(env, CARGO_TARGET_DIR=str(target), GATE_READY=str(ready)),
                                      stdin=subprocess.PIPE, stdout=output, stderr=output, text=True)
            try:
                deadline = time.monotonic() + 5
                while not ready.exists() and holder.poll() is None and time.monotonic() < deadline:
                    time.sleep(0.01)
                assert ready.exists(), f"{entry} did not reach its recording setup tool"
                for second in entries:
                    reached = root / "contender.ready"
                    result = attempt(second, alias, reached)
                    assert result.returncode == 2 and f"target folder {target}" in result.stderr, result
                    assert not reached.exists(), "a competing gate reached setup before locking"
                    print(f"PASS: {entry} excludes {second} on the same target, including an alias")
                reached = root / (entry + ".independent")
                attempt(entry, root / (entry + ".target"), reached)
                assert reached.exists(), "a different target was blocked"
                print(f"PASS: {entry} permits a different target concurrently")
            finally:
                holder.communicate("release\n", timeout=5)
        for second in entries:
            reached = root / (entry + second + ".released")
            attempt(second, target, reached)
            assert reached.exists(), "the exited gate left its target locked"
        print(f"PASS: {entry} releases its lock on exit")
