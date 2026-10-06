#!/usr/bin/env python3
"""Independent exit/EOF observation, with separately awaited finite fixtures.

A kernel lock held for a fixture's lifetime proves completion even after KILL,
when a final marker is impossible. No observer signals a descendant.
"""
import concurrent.futures
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import subprocess
import sys
import threading
import time
import uuid

WINDOWS = os.name == "nt"
OBSERVE, CLEANUP, LIFETIME = 15, 60, 45
NAME, STALE = "heartbeat-fixture", "2000-01-01T00:00:00Z"


def lock(file):
    file.seek(0)
    if WINDOWS:
        import msvcrt
        msvcrt.locking(file.fileno(), msvcrt.LK_NBLCK, 1)
    else:
        import fcntl
        fcntl.flock(file, fcntl.LOCK_EX | fcntl.LOCK_NB)


def event(root, nonce, kind):
    with (root / "events").open("a") as file:
        file.write(f"{nonce} {kind} {os.getpid()} {time.monotonic()}\n")


def fixture(root, nonce, mode):
    owner = (root / "lifetime").open("r+b")
    lock(owner)
    if mode != "deaf":
        def cancelled(_sig, _frame):
            event(root, nonce, "cancelled")
            event(root, nonce, "completed")
            sys.exit(143)
        signal.signal(signal.SIGTERM, cancelled)
    event(root, nonce, "ready")
    if mode == "native":
        print(": native-stdout-token", flush=True)
        print("native-stderr-token", file=sys.stderr, flush=True)
    deadline = time.monotonic() + LIFETIME
    while time.monotonic() < deadline:
        if mode == "release" and (root / "release").exists():
            event(root, nonce, "released")
            print(f"NAME={NAME}", flush=True)
            event(root, nonce, "completed")
            return
        time.sleep(0.01)
    event(root, nonce, "completed")


def executable(path, text):
    path.write_text(text)
    path.chmod(0o700)


def q(value):
    return shlex.quote(str(value).replace("\\", "/"))


def events(root, nonce):
    path = root / "events"
    rows = [line.split() for line in path.read_text().splitlines()] if path.exists() else []
    assert all(row[0] == nonce for row in rows), "foreign fixture event"
    return rows


class Observation:
    def __init__(self, command, env, cwd):
        self.started = time.monotonic()
        self.child = subprocess.Popen(command, env=env, cwd=cwd, stdin=subprocess.PIPE,
                                      stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.pid = self.child.pid
        self.outputs = [bytearray(), bytearray()]
        self.eofs = [threading.Event(), threading.Event()]
        self.threads = []
        for index, stream in enumerate((self.child.stdout, self.child.stderr)):
            thread = threading.Thread(target=self.drain, args=(index, stream))
            thread.start()
            self.threads.append(thread)
        try:
            self.child.stdin.write(b'{"tool_name":"Bash"}')
            self.child.stdin.close()
        except BrokenPipeError:
            pass  # Early no-op hooks need not consume their envelope.

    def drain(self, index, stream):
        while True:
            data = stream.read(4096)
            if not data:
                break
            self.outputs[index].extend(data)
        stream.close()
        self.eofs[index].set()

    def complete(self):
        return self.child.poll() == 0 and all(eof.is_set() for eof in self.eofs)

    def facts(self):
        return (self.child.poll(), *(eof.is_set() for eof in self.eofs))

    def cleanup(self, deadline):
        while time.monotonic() < deadline:
            if self.child.poll() is not None and all(eof.is_set() for eof in self.eofs):
                for thread in self.threads:
                    thread.join()
                return True
            time.sleep(0.01)
        # Only the directly created, recorded hook is eligible for termination.
        if self.child.poll() is None:
            self.child.terminate()
        return False


def seed(root, stage, mode, nonce, bash):
    home, flat = root / "comm", root / "hooks with ' quote"
    home.mkdir()
    flat.mkdir()
    for name in ("state", "self", "inbox", "read"):
        (home / name).mkdir()
    (root / "lifetime").write_bytes(b"0")
    for part in stage.glob("comm-lib*.sh"):
        shutil.copyfile(part, flat / part.name)
    shutil.copyfile(stage / "comm-status-heartbeat.sh", flat / "comm-status-heartbeat.sh")
    # Only the gate is replaced; every normal case uses the actual shared bound.
    wrapper = f". {q(stage / 'comm-lib.sh')} || return 127\n"
    wrapper += f"sot_require_agent() {{ printf '%s\\n' '{nonce} gate' >> {q(root / 'events')}; return 0; }}\n"
    if mode == "artifact-create":
        wrapper = wrapper.replace("return 0;", 'mkdir -p "$COMM_HOME/state/.hb-ctx-$$"; return 0;')
    if mode == "setup":
        wrapper += "sot_bounded() { echo 'injected bound setup failure' >&2; return 125; }\n"
    if mode == "child-gate":
        wrapper += "sot_require_agent() { return 1; }\n"
    (flat / "comm-lib.sh").write_text(wrapper)
    if mode != "fallback":
        (home / "bin").mkdir()
        shutil.copyfile(flat / "comm-lib.sh", home / "bin/comm-lib.sh")
    row = dict(state="working", floor="working", question="q", waiting="w", note="n",
               summary="s", status_at=STALE, last_seen=STALE)
    if mode == "absent-floor":
        row.pop("floor")
    if mode == "empty-floor":
        row["floor"] = ""
    registry = {"agents": {NAME: row} if mode != "no-row" else {}}
    (home / "registry.json").write_text(json.dumps(registry))
    env = os.environ.copy()
    env.update(SOT_COMM_HOME=str(home), CLAUDE_CODE_SESSION_ID=nonce)
    for key in ("SOT_COMM_NAME", "SOT_COMM_SELF_FILE", "SOT_WORKSPACE_ID", "SOT_COMM_HOOKS"):
        env.pop(key, None)
    target = flat / "comm-context.sh"
    entry = f"printf '%s\\n' '{nonce} entry' >> {q(root / 'events')}\n"
    invoke = f"{q(sys.executable)} -B {q(Path(__file__).resolve())} --fixture {q(root)} {q(nonce)}"
    script = f"#!{bash}\n{entry}"
    if mode in ("foreground", "native"):
        script += "trap " + q(f"printf '%s\\n' '{nonce} trapped' >> {q(root / 'events')}; exit 143") + f" TERM\necho NAME={NAME}\n"
        script += invoke + (" native\n" if mode == "native" else " hold\n")
    elif mode in ("deaf", "zero", "overflow"):
        script += "trap '' TERM\nexec " + invoke + " deaf\n"
    elif mode == "leader":
        script += f"echo NAME={NAME}\n{invoke} hold &\n"
        script += f"while ! test -f {q(root / 'child-ready')}; do sleep 0.01; done\nexit 0\n"
    elif mode in ("release", "default-timeout"):
        script += "exec " + invoke + " release\n"
    else:
        if mode == "artifact-remove":
            script += f"chmod 500 {q(home / 'state')}\n"
        if mode == "diagnostics":
            script += "echo completed-diagnostic-token >&2\n"
        script += f"echo NAME={NAME if mode != 'empty-name' else ''}\nexit {7 if mode == 'nonzero' else 1 if mode == 'exit-one' else 0}\n"
    executable(target, script)
    if mode == "real":
        shutil.copyfile(stage / "comm-context.sh", target)
        subprocess.run(["git", "init", "-q", str(root / "project")], check=True)
        self_file = home / "self/fixture.txt"
        project = str((root / "project").resolve()).replace("\\", "/")
        self_file.write_text(f"{NAME}\nrepo=project\nroot={project}\n")
        env.update(SOT_COMM_SELF_FILE=str(self_file), SOT_COMM_TEST_HOST="fixture")
    if mode == "missing":
        target.unlink()
    if mode == "off":
        env["SOT_COMM_HOOKS"] = "off"
    if mode == "no-registry":
        (home / "registry.json").unlink()
    if mode == "throttle":
        (home / f"state/hb-{nonce}.tick").touch()
    return home, flat, env


def fixture_finished(root, rows):
    if not any(row[1] == "ready" for row in rows):
        return True
    try:
        with (root / "lifetime").open("r+b") as file:
            lock(file)
        return True
    except OSError:
        return False


def validate(mode, observed, entries, stamped, unchanged, artifacts, stderr):
    failures = {
        "foreground": "foreground child kept heartbeat waiting",
        "deaf": "TERM-deaf context kept heartbeat waiting",
        "leader": "unfinished context group stamped the row",
        "setup": "context ran without its bound",
        "diagnostics": "completed context diagnostics were lost",
        "zero": "zero budget started context",
        "overflow": "unusable numeric budget started context",
        "release": "positive subsecond budget cancelled a releasable context",
        "artifact-remove": "removal failure stamped the row",
    }
    assert observed, failures.get(mode, "hook exit and both EOFs were not observed")
    early = mode in ("off", "no-registry", "child-gate", "throttle", "missing", "zero", "overflow", "setup", "artifact-create")
    if mode != "real":
        assert entries == (0 if early else 1), failures.get(mode, "incorrect fixture entry count")
    skip = early or mode in ("foreground", "deaf", "leader", "empty-name", "no-row", "absent-floor", "empty-floor", "native", "default-timeout", "artifact-remove")
    assert unchanged if skip else stamped, failures.get(mode, "registry stamp preservation failed")
    assert not artifacts or mode == "artifact-remove", "owned context artifacts remained"
    if mode in ("setup", "foreground", "deaf", "leader", "native"):
        assert "heartbeat context unavailable (command bound status" in stderr, "bound diagnostic missing"
    if mode == "diagnostics":
        assert stderr.count("completed-diagnostic-token") == 1, failures[mode]
    if mode == "overflow":
        assert "unusable" in stderr, "unusable budget diagnostic missing"


def run_case(work, stage, bash, label, mode, ticks, delay=0):
    nonce = uuid.uuid4().hex
    root = work / nonce
    root.mkdir()
    home, flat, env = seed(root, stage, mode, nonce, bash)
    if ticks is not None:
        env["SOT_HB_CTX_TIMEOUT_TICKS"] = ticks
    else:
        env.pop("SOT_HB_CTX_TIMEOUT_TICKS", None)
    registry = home / "registry.json"
    before = registry.read_bytes() if registry.exists() else b""
    command = [bash, str(flat / "comm-status-heartbeat.sh")]
    if mode in ("zero", "overflow"):
        wrapper = root / "ignore-term.sh"
        executable(wrapper, f"#!{bash}\ntrap '' TERM\nexec {q(bash)} {q(flat / 'comm-status-heartbeat.sh')}\n")
        command = [bash, str(wrapper)]
    observation = Observation(command, env, root / "project" if mode == "real" else root)
    ready_at, released_at = None, None
    observation_deadline = observation.started + OBSERVE
    while time.monotonic() < observation_deadline:
        rows = events(root, nonce)
        if any(row[1] == "ready" for row in rows) and ready_at is None:
            ready_at = time.monotonic()
            observation_deadline = ready_at + OBSERVE
            (root / "child-ready").touch()
        if mode == "release" and ready_at and not released_at and time.monotonic() >= ready_at + delay:
            (root / "release").touch()
            released_at = time.monotonic()
        if observation.complete() and (mode != "release" or released_at):
            break
        time.sleep(0.01)
    observed, facts = observation.complete(), observation.facts()
    rows = events(root, nonce)
    entries = sum(row[1] == "entry" for row in rows)
    after = registry.read_bytes() if registry.exists() else b""
    stamped = False
    if before != after and registry.exists():
        old, new = json.loads(before)["agents"][NAME], json.loads(after)["agents"][NAME]
        stamped = new["status_at"] != STALE and new["last_seen"] != STALE
        keep = lambda row: {k: v for k, v in row.items() if k not in ("status_at", "last_seen")}
        stamped &= keep(old) == keep(new)
    artifacts = [path for path in (home / "state").glob(".hb-ctx-*") if path.is_file()]
    if mode == "repeat":
        again = Observation(command, env, root)
        assert again.cleanup(time.monotonic() + CLEANUP) and again.complete(), "repeat completion failed"
        assert events(root, nonce) == rows, "repeat key started context"
    if mode == "default-timeout":
        assert time.monotonic() - observation.started >= 10, "default deadline ended early"
    stderr = observation.outputs[1].decode(errors="replace")
    failure = None
    try:
        if mode in ("foreground", "deaf", "leader", "native", "release", "zero", "overflow") and entries:
            assert ready_at is not None, "fixture body readiness missing"
        if mode == "release":
            assert released_at is not None, "release handshake missing"
            rounded = (int(ticks or "200") + 19) // 20 if (ticks or "200").isdigit() else 10
            assert ready_at - observation.started + delay < rounded, "readiness exhausted the rounded head budget"
        validate(mode, observed, entries, stamped, before == after, artifacts, stderr)
    except AssertionError as error:
        failure = str(error)
        if label == "B3-21" and "subsecond" in failure:
            failure = "fractional second budget cancelled a releasable context"
    if mode == "artifact-create":
        (home / f"state/.hb-ctx-{observation.pid}").rmdir()
    if mode == "artifact-remove":
        (home / "state").chmod(0o700)
        for path in artifacts:
            path.unlink()
    # Preserve the verdict before cleanup. Later exit/EOF never repairs it.
    deadline = time.monotonic() + CLEANUP
    pipe_cleanup = observation.cleanup(deadline)
    while time.monotonic() < deadline and not fixture_finished(root, events(root, nonce)):
        time.sleep(0.01)
    cleanup = pipe_cleanup and fixture_finished(root, events(root, nonce))
    cleanup &= not list((home / "state").glob(".hb-ctx-*"))
    if not cleanup:
        raise RuntimeError(f"STOP {label}: fixture cleanup not confirmed")
    print(f"{label} nonce={nonce} interpreter={'Git Bash/native Python' if WINDOWS else 'Unix Bash'} entry={entries} exit={facts[0]} stdout-EOF={facts[1]} stderr-EOF={facts[2]} cleanup={cleanup}", flush=True)
    if ready_at:
        release_time = released_at - ready_at if released_at else "none"
        print(f"{label} invocation-to-readiness={ready_at - observation.started:.3f} release-after-readiness={release_time} events={','.join(row[1] for row in rows)}", flush=True)
    print(f"{'FAIL' if failure else 'PASS'} {label}: {failure or 'required registry, capture and cleanup outcomes'}", flush=True)
    return failure


def sensitivity(work, bash):
    cases = [("missing-entry", "exit 0", "entry"),
             ("retained-stdout", "sleep 2 & exit 0", "stdout"),
             ("retained-stderr", "sleep 2 >&2 1>/dev/null & exit 0", "stderr"),
             ("forced-hang", "sleep 2", "exit"),
             ("retained-artifact", "exit 0", "artifact")]
    for label, script, predicate in cases:
        root = work / label
        root.mkdir()
        if predicate == "artifact":
            (root / ".hb-ctx-owned").touch()
        observation = Observation([bash, "-c", script], os.environ.copy(), root)
        time.sleep(0.2)
        actual = {"entry": False, "stdout": observation.eofs[0].is_set(),
                  "stderr": observation.eofs[1].is_set(), "exit": observation.child.poll() == 0,
                  "artifact": not (root / ".hb-ctx-owned").exists()}[predicate]
        assert not actual, f"sensitivity failed: {label}"
        assert observation.cleanup(time.monotonic() + CLEANUP), "sensitivity cleanup failed"
        if predicate == "artifact":
            (root / ".hb-ctx-owned").unlink()
        print(f"PASS sensitivity {label}: intended {predicate} assertion failed; separate cleanup confirmed", flush=True)


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--fixture":
        fixture(Path(sys.argv[2]), sys.argv[3], sys.argv[4])
        return 0
    work, stage = map(lambda value: Path(value).resolve(), sys.argv[1:3])
    bash = shutil.which("bash")
    assert bash and shutil.which("jq") and shutil.which("perl"), "FATAL: bash, jq and Perl are required"
    sentinel = work / "outside-sentinel"
    sentinel.write_text("untouched")
    cases = [("R1", "foreground", "20"), ("R2", "foreground" if WINDOWS else "deaf", "20"),
             ("R3", "leader", "20"), ("R4", "setup", "20"), ("diagnostics", "diagnostics", "20"),
             ("B1-0", "zero", "0"), ("B1-000", "zero", "000"),
             ("B2-10", "release", "10", 0.7), ("B2-0010", "release", "0010", 0.7),
             ("B3-21", "release", "21", 1.5), ("B4", "overflow", "999999999999999999999999999999999999")]
    cases += [(mode, mode, "20") for mode in ("success", "nonzero", "exit-one", "repeat", "artifact-create", "fallback", "real", "empty-name", "missing", "off", "no-registry", "child-gate", "throttle", "no-row", "absent-floor", "empty-floor")]
    cases += [("budget-" + label, "release", ticks, 1.5) for label, ticks in (("unset", None), ("200", "200"), ("empty", ""), ("invalid", "invalid"))]
    cases += [("budget-" + ticks, "success", ticks) for ticks in ("20", "020", "9223372036854775807")]
    cases += [("deadline-" + label, "default-timeout", ticks) for label, ticks in (("unset", None), ("200", "200"), ("empty", ""), ("invalid", "invalid"))]
    if not WINDOWS:
        cases.append(("artifact-remove", "artifact-remove", "20"))
    if WINDOWS:
        print("Coverage: POSIX directory-mode removal fault is Unix only", flush=True)
        cases.append(("P5", "native", "20"))
    selected = set(sys.argv[3:])
    if selected:
        cases = [case for case in cases if case[0] in selected]
        assert cases, "no selected case exists"
    failures = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as executor:
        jobs = [executor.submit(run_case, work, stage, bash, *case) for case in cases]
        for case, job in zip(cases, jobs):
            failure = job.result()
            if failure:
                failures.append((case[0], failure))
    sensitivity(work, bash)
    assert sentinel.read_text() == "untouched", "outside scratch sentinel changed"
    (work / "cleanup-confirmed").touch()
    print(f"heartbeat: {len(cases) - len(failures)} passed, {len(failures)} failed; exit/both EOFs/cleanup observed separately", flush=True)
    if WINDOWS:
        print("P5 Q1: native termination requires review of lifetime/completion evidence; ignored TERM coverage is Unix only", flush=True)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
