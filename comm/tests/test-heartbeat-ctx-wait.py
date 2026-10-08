#!/usr/bin/env python3
"""Independent exit/EOF observation, with separately awaited finite fixtures.

A kernel lock held for a fixture's lifetime proves completion even after KILL,
when a final marker is impossible. No observer signals a descendant.
"""
import concurrent.futures
import copy
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
OBSERVATIONS = []


def lock(file):
    file.seek(0)
    if WINDOWS:
        import msvcrt
        msvcrt.locking(file.fileno(), msvcrt.LK_NBLCK, 1)
    else:
        import fcntl
        fcntl.flock(file, fcntl.LOCK_EX | fcntl.LOCK_NB)


def event(root, nonce, kind):
    with (root / "events").open("ab") as file:
        file.write(f"{nonce} {kind} {os.getpid()} {time.monotonic()}\n".encode())


def protocol(text, stream=None):
    stream = stream or sys.stdout
    stream.buffer.write(text.encode("utf-8"))
    stream.buffer.flush()


def fixture(root, nonce, mode):
    owner = (root / "lifetime").open("r+b")
    lock(owner)
    if mode != "deaf":
        def cancelled(_sig, _frame):
            event(root, nonce, "cancelled")
            event(root, nonce, "completed")
            sys.exit(143)
        signal.signal(signal.SIGTERM, cancelled)
    event(root, nonce, "ready-native")
    if mode == "native":
        protocol(": native-stdout-token\n")
        protocol("native-stderr-token\n", sys.stderr)
        event(root, nonce, "tokens")
    deadline = time.monotonic() + LIFETIME
    while time.monotonic() < deadline:
        if mode == "release" and (root / "release").exists():
            event(root, nonce, "released")
            protocol(f"NAME={NAME}\n")
            event(root, nonce, "completed")
            return
        time.sleep(0.01)
    event(root, nonce, "self-expired")
    event(root, nonce, "completed")


def executable(path, text):
    path.write_bytes(text.encode("utf-8"))
    path.chmod(0o700)


def q(value):
    return shlex.quote(str(value).replace("\\", "/") if isinstance(value, Path) else str(value))


def events(root, nonce):
    path = root / "events"
    rows = [line.decode().split() for line in path.read_bytes().split(b"\n")[:-1]] if path.exists() else []
    assert all(row[0] == nonce for row in rows), "foreign fixture event"
    return rows


class Observation:
    def __init__(self, command, env, cwd):
        self.root = cwd.parent if cwd.name == "project" else cwd
        OBSERVATIONS.append(self)
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
    if mode in ("artifact-create", "artifact-create-stderr"):
        suffix = ".err" if mode.endswith("stderr") else ""
        fault = f'$COMM_HOME/state/.hb-ctx-$${suffix}'
        receipt = q(root / "artifact-receipt")
        wrapper = wrapper.replace("return 0;", f'mkdir "{fault}" && '
                                  f"printf '%s\\n%s\\n' {q(nonce)} \"{fault}\" > {receipt}; return 0;")
    if mode == "setup":
        wrapper += f"sot_bounded() {{ printf '%s\\n' '{nonce} bound-setup' >> {q(root / 'events')}; echo 'injected bound setup failure' >&2; return 125; }}\n"
    if mode == "child-gate":
        wrapper += "sot_require_agent() { return 1; }\n"
    (flat / "comm-lib.sh").write_bytes(wrapper.encode("utf-8"))
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
    invoke = f"{q(Path(sys.executable))} -B {q(Path(__file__).resolve())} --fixture {q(root)} {q(nonce)}"
    script = f"#!/usr/bin/env bash\n{entry}"
    if mode in ("foreground", "native"):
        script += "trap " + q(f"printf '%s\\n' '{nonce} trapped' >> {q(root / 'events')}; exit 143") + f" TERM\necho NAME={NAME}\n"
        script += invoke + (" native\n" if mode == "native" else " hold\n")
    elif mode in ("deaf", "zero", "overflow"):
        script += "trap '' TERM\nexec " + invoke + " deaf\n"
    elif mode == "leader":
        script += f"echo NAME={NAME}\n{invoke} hold &\n"
        script += f"while ! test -f {q(root / 'child-ready')}; do sleep 0.01; done\nexit 0\n"
    elif mode in ("release", "default-timeout"):
        script += responsive(root, nonce, invoke)
    else:
        if mode == "artifact-remove":
            script += f"chmod 500 {q(home / 'state')}\n"
        if mode == "diagnostics":
            script += "echo completed-diagnostic-token >&2\n"
        script += f"echo NAME={NAME if mode != 'empty-name' else ''}\nexit {7 if mode == 'nonzero' else 1 if mode == 'exit-one' else 0}\n"
    if mode in ("foreground", "native", "deaf", "zero", "overflow", "leader", "release", "default-timeout"):
        script = script.replace(entry, entry + f"printf '%s\\n' '{nonce} launch' >> {q(root / 'events')}\n", 1)
    if mode in ("foreground", "native", "deaf", "zero", "overflow"):
        script = script.replace(entry, entry + f"printf '%s\\n' '{nonce} disposition' >> {q(root / 'events')}\n", 1)
    executable(target, script)
    if mode == "real":
        shutil.copyfile(stage / "comm-context.sh", target)
        subprocess.run(["git", "init", "-q", str(root / "project")], check=True)
        self_file = home / "self/fixture.txt"
        probe = subprocess.run([bash, "-c", f". {q(stage / 'comm-lib.sh')} && cd {q(root / 'project')} && "
                                "sot_canonical_path \"$(git rev-parse --show-toplevel)\""],
                               capture_output=True, timeout=30, check=True)
        project = probe.stdout.decode().strip()
        self_file.write_bytes(f"{NAME}\nrepo=project\nroot={project}\n".encode())
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


def ready_rows(rows):
    return [row for row in rows if (row[1].startswith("ready-") or row[1] == "ready")]


def responsive(root, nonce, invoke):
    # Bash inherits the lifetime lock across exec; the witness adds no signal hop.
    # The finite holding body uses Bash/coreutils; it has no native signal premise.
    body = root / "responsive.sh"
    record = f"printf '%s %s %s %s\\n' {q(nonce)}"
    destination = q(root / "events")
    emit = lambda kind: f"{record} {kind} \"$$\" \"$SECONDS\" >> {destination}"
    cancellation = f"printf '%s cancelled %s %s\\n%s completed %s %s\\n' {q(nonce)} \"$$\" \"$SECONDS\" {q(nonce)} \"$$\" \"$SECONDS\" >> {destination}; exit 143"
    executable(body, f"#!/usr/bin/env bash\n"
               f"trap {q(cancellation)} TERM\n"
               f"{emit('ready-msys' if WINDOWS else 'ready-bash')}\n"
               f"while [ \"$SECONDS\" -lt {LIFETIME} ]; do\n"
               f"  if [ -f {q(root / 'release')} ]; then {emit('released')}; "
               f"printf '%s\\n' NAME={NAME}; {emit('completed')}; exit 0; fi\n"
               f"done\n{emit('self-expired')}; {emit('completed')}\n")
    (root / "lifetime-perl").touch()
    program = ("use Fcntl qw(:flock F_SETFD); open(my $f, '+<', $ARGV[0]) or die $!; "
               "flock($f, LOCK_EX) or die $!; fcntl($f, F_SETFD, 0) or die $!; "
               "exec 'bash', $ARGV[1]; die $!;")
    return f"exec perl -e {q(program)} {q(root / 'lifetime')} {q(body)}\n"


def fixture_finished(root, rows):
    if not ready_rows(rows):
        return False
    if (root / "lifetime-perl").exists():
        result = subprocess.run(["perl", "-MFcntl=:flock", "-e",
                                 "open(my $f, '+<', $ARGV[0]) or exit 2; "
                                 "exit(flock($f, LOCK_EX|LOCK_NB) ? 0 : 1)",
                                 str(root / "lifetime")], timeout=5)
        return result.returncode == 0
    try:
        with (root / "lifetime").open("r+b") as file:
            lock(file)
        return True
    except OSError:
        return False


def certify(root, rows, observation, artifacts):
    lifetime = fixture_finished(root, rows) if any(r[1] == "launch" for r in rows) else observation.complete()
    return (lifetime and observation.child.poll() is not None and
            all(e.is_set() for e in observation.eofs) and
            all(not t.is_alive() for t in observation.threads) and not artifacts)


def certified_observations():
    for observation in OBSERVATIONS:
        root = observation.root
        rows = events(root, root.name)
        if not certify(root, rows, observation, []):
            return False
    return True


def creator_artifact(root, nonce):
    receipt = root / "artifact-receipt"
    assert receipt.exists(), "creator artifact receipt missing"
    lines = receipt.read_bytes().split(b"\n")
    assert len(lines) == 3 and lines[-1] == b"" and lines[0].decode() == nonce, "creator artifact receipt malformed or foreign"
    raw = lines[1].decode("utf-8")
    if WINDOWS and raw.startswith("/"):
        raw = subprocess.check_output(["cygpath", "-w", raw], timeout=5).decode().strip()
    path = Path(raw)
    state = (root / "comm/state").resolve()
    assert path.is_absolute() and path.parent.resolve() == state, "creator artifact receipt outside owned state"
    assert path.name.startswith(".hb-ctx-") and path.is_dir(), "creator artifact receipt is not its owned directory"
    return path


def remove_creator_artifact(root, nonce, pid):
    path = creator_artifact(root, nonce)
    path.rmdir()
    assert not path.exists(), "creator-owned artifact was not removed"


def validate(mode, observed, entries, stamped, unchanged, artifacts, stderr):
    # Invocation assertions precede completion: a waiting prohibited call is red.
    entry_errors = {"zero": "zero budget started context", "overflow": "unusable numeric budget started context",
                    "setup": "context ran without its bound", "artifact-create": "context ran after stdout artifact creation failure",
                    "artifact-create-stderr": "context ran after stderr artifact creation failure"}
    early = mode in ("off", "no-registry", "child-gate", "throttle", "missing", *entry_errors)
    if mode in entry_errors and entries:
        raise AssertionError(entry_errors[mode])
    assert observed, "OBSERVATION: hook exit and both EOFs were not observed"
    if mode != "real":
        assert entries == (0 if early else 1), "FIXTURE: context entry assertion rejected corrupted evidence"
    skip = early or mode in ("foreground", "deaf", "leader", "empty-name", "no-row", "absent-floor", "empty-floor", "native", "default-timeout", "artifact-remove")
    registry_error = {"leader": "unfinished context group stamped the row", "artifact-remove": "removal failure stamped the row"}
    assert unchanged if skip else stamped, registry_error.get(mode, "registry stamp preservation failed")
    assert not artifacts or mode == "artifact-remove", "artifact assertion rejected retained owned path"
    if mode in ("setup", "foreground", "deaf", "leader", "native"):
        assert "heartbeat context unavailable (command bound status" in stderr, "bound diagnostic missing"
    if mode == "diagnostics":
        assert stderr.count("completed-diagnostic-token") == 1, "completed context diagnostics were lost"
    if mode == "overflow":
        assert "unusable" in stderr, "unusable budget diagnostic missing"
    if mode in ("artifact-create-stderr", "artifact-remove"):
        assert "heartbeat context artifact could not be created or removed; heartbeat skipped" in stderr, "artifact diagnostic missing"


def release_valid(f):
    ticks = f["ticks"] or "200"
    rounded = (int(ticks) + 19) // 20 if ticks.isdigit() else 10
    # The command bound starts at the context's entry; hook start-up before it is not budget.
    origin = f["stamps"].get("entry", f["started"])
    assert f["released_at"] is not None, "FIXTURE: release handshake missing"
    assert f["released_at"] - origin < rounded, "FIXTURE: actual release exhausted the rounded head budget"
    assert f["released_at"] - origin >= f["delay"], "FIXTURE: release preceded the requested delay"
    if any(r[1] == "released" for r in f["rows"]):
        assert f["stamps"]["released"] - origin < rounded, "FIXTURE: release consumption exhausted the rounded head budget"


def case_verdict(f):
    mode, rows = f["mode"], f["rows"]
    if mode in ("zero", "overflow", "setup", "artifact-create", "artifact-create-stderr") and f["entries"]:
        validate(mode, f["observed"], f["entries"], f["stamped"], f["unchanged"], f["artifacts"], f["stderr"])
    if mode in ("foreground", "deaf", "leader", "native", "release", "default-timeout", "zero", "overflow") and f["entries"]:
        assert ready_rows(rows), "FIXTURE: fixture body readiness missing"
    if mode in ("foreground", "deaf", "native") and not f["observed"]:
        assert any(r[1] == "launch" for r in rows), "FIXTURE: fixture launch intent missing"
        assert any(r[1] == "disposition" for r in rows), "FIXTURE: TERM disposition evidence missing"
        message = {"foreground": "foreground child kept heartbeat waiting", "deaf": "TERM-deaf context kept heartbeat waiting",
                   "native": "native foreground child kept heartbeat waiting"}[mode]
        raise AssertionError(message)
    if mode == "leader" and not f["unchanged"]:
        assert not f["lifetime_finished"], "FIXTURE: stamped descendant already finished"
    if mode == "setup" and not f["entries"]:
        assert any(r[1] == "bound-setup" for r in rows), "FIXTURE: bound setup receipt missing"
    if mode in ("release", "default-timeout"):
        assert f["entries"] == 1 and ready_rows(rows), "FIXTURE: responsive context entry/readiness missing"
        assert ready_rows(rows)[0][1] == ("ready-msys" if WINDOWS else "ready-bash"), "budget cancellation lacks a TERM-responsive MSYS receipt"
        if mode == "release":
            release_valid(f)
            kinds = [r[1] for r in rows if r[1] in ("released", "cancelled", "completed", "self-expired")]
            if kinds == ["cancelled", "completed"]:
                assert f["observed"] and f["unchanged"], "OBSERVATION: cancellation capture/registry incomplete"
                raise AssertionError("fractional second budget cancelled a releasable context" if f["label"] == "B3-21" else
                                     "positive subsecond budget cancelled a releasable context")
            assert kinds == ["released", "completed"], "FIXTURE: responsive release/completion order invalid"
        else:
            assert [r[1] for r in rows if r[1] in ("cancelled", "completed")] == ["cancelled", "completed"], "FIXTURE: default cancellation absent"
            assert f["cutoff"] - f["started"] >= 10, "FIXTURE: default deadline ended early"
    if mode == "native":
        assert "native-stderr-token" in f["stderr"], "FIXTURE: native stderr token missing"
        assert any(r[1] == "tokens" for r in rows), "FIXTURE: native stream tokens not emitted"
    for stream in ("exit", "stdout", "stderr"):
        stream_assertion(f, stream)
    validate(mode, f["observed"], f["entries"], f["stamped"], f["unchanged"], f["artifacts"], f["stderr"])


def capture_case(root, home, observation, before, label, mode, ticks, delay):
    ready_at, released_at, stamps = None, None, {}
    deadline = observation.started + OBSERVE
    while time.monotonic() < deadline:
        rows = events(root, root.name)
        now = time.monotonic()
        for row in rows:
            stamps.setdefault(row[1], now)
        if "entry" in stamps and ready_at is None:
            deadline = max(deadline, stamps["entry"] + OBSERVE)  # slow start-up precedes the context
        if ready_rows(rows) and ready_at is None:
            ready_at = now
            deadline = ready_at + OBSERVE
            (root / "child-ready").touch()
        if mode == "release" and ready_at and not released_at and now >= stamps["entry"] + delay:
            (root / "release").touch()
            released_at = time.monotonic()
        if observation.complete() and (mode != "release" or released_at):
            break
        time.sleep(0.01)
    cutoff = time.monotonic()
    rows = events(root, root.name)
    registry = home / "registry.json"
    after = registry.read_bytes() if registry.exists() else b""
    stamped = False
    if before != after and registry.exists():
        old, new = json.loads(before)["agents"][NAME], json.loads(after)["agents"][NAME]
        keep = lambda row: {k: v for k, v in row.items() if k not in ("status_at", "last_seen")}
        stamped = new["status_at"] != STALE and new["last_seen"] != STALE and keep(old) == keep(new)
    return dict(label=label, mode=mode, ticks=ticks, delay=delay, rows=rows,
                entries=sum(r[1] == "entry" for r in rows), observed=observation.complete(),
                facts=observation.facts(), started=observation.started, ready_at=ready_at,
                released_at=released_at, cutoff=cutoff, stamps=stamps, stamped=stamped,
                unchanged=before == after, artifacts=[p for p in (home / "state").glob(".hb-ctx-*") if p.is_file()],
                stderr=observation.outputs[1].decode(errors="replace"),
                lifetime_finished=fixture_finished(root, rows) if ready_rows(rows) else False)


def cleanup_case(root, home, mode, observation):
    if mode in ("artifact-create", "artifact-create-stderr"):
        remove_creator_artifact(root, root.name, observation.pid)
    if mode == "artifact-remove":
        (home / "state").chmod(0o700)
    deadline = time.monotonic() + CLEANUP
    assert observation.cleanup(deadline), "STOP: hook/drain cleanup not confirmed"
    while time.monotonic() < deadline:
        rows = events(root, root.name)
        if not any(r[1] == "launch" for r in rows) or fixture_finished(root, rows):
            break
        time.sleep(0.01)
    # Product-reversal artifacts are accounted for only after all owners finish.
    paths = list((home / "state").glob(".hb-ctx-*"))
    for path in paths:
        assert path.is_file(), "STOP: unknown context artifact"
        path.unlink()
    assert certify(root, rows, observation, list((home / "state").glob(".hb-ctx-*"))), "STOP: fixture cleanup not confirmed"
    return rows


def run_case(work, stage, bash, label, mode, ticks, delay=0):
    root = work / uuid.uuid4().hex
    root.mkdir()
    home, flat, env = seed(root, stage, mode, root.name, bash)
    if ticks is None:
        env.pop("SOT_HB_CTX_TIMEOUT_TICKS", None)
    else:
        env["SOT_HB_CTX_TIMEOUT_TICKS"] = ticks
    registry = home / "registry.json"
    before = registry.read_bytes() if registry.exists() else b""
    command = [bash, str(flat / "comm-status-heartbeat.sh")]
    if mode in ("zero", "overflow"):
        wrapper = root / "ignore-term.sh"
        executable(wrapper, f"#!/usr/bin/env bash\ntrap '' TERM\nexec {q(Path(bash))} {q(flat / 'comm-status-heartbeat.sh')}\n")
        command = [bash, str(wrapper)]
    observation = Observation(command, env, root / "project" if mode == "real" else root)
    f = capture_case(root, home, observation, before, label, mode, ticks, delay)
    failure = None
    try:
        case_verdict(f)
    except AssertionError as error:
        failure = str(error)
    final = cleanup_case(root, home, mode, observation)
    if mode == "repeat":
        again = Observation(command, env, root)
        assert again.cleanup(time.monotonic() + CLEANUP) and again.complete(), "repeat completion failed"
        assert events(root, root.name) == final, "repeat key started context"
    f["cleanup"] = True
    f["failure"] = failure
    print(f"{label} nonce={root.name} entry={f['entries']} exit={f['facts'][0]} stdout-EOF={f['facts'][1]} stderr-EOF={f['facts'][2]} cleanup=True", flush=True)
    print(f"{label} timing={dict(invocation=f['started'], readiness=f['ready_at'], release_send=f['released_at'], cutoff=f['cutoff'], events=f['stamps'])} events={','.join(r[1] for r in f['rows'])}", flush=True)
    if failure:
        print(f"{label} stderr-tail={f['stderr'][-600:]!r}", flush=True)
    print(f"{'FAIL' if failure else 'PASS'} {label}: {failure or 'required registry, capture and cleanup outcomes'}", flush=True)
    if mode == "release":
        kinds = [r[1] for r in f["rows"] if r[1] in ("cancelled", "released", "completed")]
        f["budget_order"] = kinds
    if mode == "native":
        end = "self-expiry" if any(r[1] == "self-expired" for r in final) else "termination"
        print(f"P5 native-ended-at-observation={f['lifetime_finished']} native-cleanup=True native-end={end}", flush=True)
        if not failure:
            print("P5 capture: entry and child readiness observed; hook exit 0; stdout EOF; stderr EOF; no stamp; no artifacts")
            print("P5 cleanup: native lifetime completion confirmed separately; both EOFs confirmed")
            print("P5 Q1: native termination disposition remains a separate acceptance gate")
    evidence = {k: v for k, v in f.items() if k not in ("stderr", "artifacts")}
    evidence["artifacts"] = len(f["artifacts"])
    print("CASE " + json.dumps(evidence), flush=True)
    return f


def rejected(call, expected):
    try:
        call()
    except AssertionError as error:
        assert str(error) == expected, f"wrong assertion: {error}; expected {expected}"
        return
    raise AssertionError(f"assertion did not reject: {expected}")


def await_fact(predicate):
    deadline = time.monotonic() + OBSERVE
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.01)
    raise AssertionError("FIXTURE: handshake not observed")


def pipe_counterexample(work, bash, label):
    root = work / label
    root.mkdir()
    hold = root / "hold.sh"
    nonce = root.name
    # Parent waits for acknowledged body entry; observer witnesses its process exit.
    executable(hold, f"#!/usr/bin/env bash\nprintf '%s' ready > {q(root / 'ready')}\n"
               f"while [ ! -f {q(root / 'release')} ] && [ \"$SECONDS\" -lt {LIFETIME} ]; do sleep 0.01; done\n")
    redirects = "2>/dev/null" if label == "retained-stdout" else "1>/dev/null"
    parent = (f"{q(Path(bash))} {q(hold)} {redirects} &\n"
              f"while [ ! -f {q(root / 'ready')} ]; do sleep 0.01; done\nexit 0")
    command = [bash, str(hold)] if label == "forced-hang" else [bash, "-c", parent]
    observation = Observation(command, os.environ.copy(), root)
    await_fact(lambda: (root / "ready").exists())
    if label != "forced-hang":
        await_fact(lambda: observation.child.poll() == 0)
        other = 1 if label == "retained-stdout" else 0
        await_fact(lambda: observation.eofs[other].is_set())
    return root, observation


def stream_assertion(observation, stream):
    facts = observation["facts"] if isinstance(observation, dict) else observation.facts()
    if stream == "exit":
        assert facts[0] == 0, "exit deadline assertion rejected acknowledged live parent"
    else:
        index = 0 if stream == "stdout" else 1
        assert facts[index + 1], f"{stream} EOF assertion rejected acknowledged retained pipe"


def sensitivity(work, bash):
    assert COMPANION is not None, "successful live companion missing"
    case_verdict(COMPANION)
    corrupt = copy.deepcopy(COMPANION)
    corrupt["rows"] = [r for r in corrupt["rows"] if r[1] != "entry"]
    corrupt["entries"] = sum(r[1] == "entry" for r in corrupt["rows"])
    rejected(lambda: case_verdict(corrupt), "FIXTURE: context entry assertion rejected corrupted evidence")
    print("PASS sensitivity missing-entry: context entry assertion rejected corrupted evidence")
    for label, stream in (("retained-stdout", "stdout"), ("retained-stderr", "stderr"), ("forced-hang", "exit")):
        root, observation = pipe_counterexample(work, bash, label)
        expected = f"{stream} {'deadline' if stream == 'exit' else 'EOF'} assertion rejected acknowledged {'live parent' if stream == 'exit' else 'retained pipe'}"
        rejected(lambda: stream_assertion(observation, stream), expected)
        (root / "release").touch()
        assert observation.cleanup(time.monotonic() + CLEANUP), "sensitivity cleanup unconfirmed"
        assert observation.complete(), "sensitivity completion unconfirmed"
        print(f"PASS sensitivity {label}: {expected}")
    artifact = work / ".hb-ctx-owned"
    artifact.touch()
    corrupt = copy.deepcopy(COMPANION)
    corrupt["artifacts"] = [artifact]
    rejected(lambda: case_verdict(corrupt), "artifact assertion rejected retained owned path")
    artifact.unlink()
    print("PASS sensitivity retained-artifact: artifact assertion rejected retained owned path")
    f = timing_control()
    rejected(lambda: release_valid(f), "FIXTURE: actual release exhausted the rounded head budget")
    print("PASS sensitivity actual-release: timing assertion rejected actual late release")
    readiness_control(work, bash)
    print("PASS sensitivity missing-readiness: cleanup assertion rejected unconfirmed launched fixture")


COMPANION = None
BUDGET_COMPANION = None


def timing_control():
    assert BUDGET_COMPANION is not None, "captured budget companion missing"
    f = copy.deepcopy(BUDGET_COMPANION)
    ticks = f["ticks"] or "200"
    rounded = (int(ticks) + 19) // 20 if ticks.isdigit() else 10
    f["released_at"] = f["stamps"].get("entry", f["started"]) + rounded + 0.1
    return f


def readiness_control(work, bash):
    root = work / uuid.uuid4().hex
    root.mkdir()
    (root / "lifetime").write_bytes(b"0")
    event(root, root.name, "launch")
    script = (f"printf '%s' launch > {q(root / 'launch')}\n"
              f"while [ ! -f {q(root / 'acquire')} ] && [ \"$SECONDS\" -lt {LIFETIME} ]; do sleep 0.01; done\n"
              f"exec {q(Path(sys.executable))} -B {q(Path(__file__).resolve())} --fixture {q(root)} {q(root.name)} deaf\n")
    observation = Observation([bash, "-c", script], os.environ.copy(), root)
    await_fact(lambda: (root / "launch").exists())
    rows = [[root.name, "launch"]]
    premature = fixture_finished(root, rows)
    certified = certify(root, rows, observation, [])
    (root / "acquire").touch()
    await_fact(lambda: ready_rows(events(root, root.name)))
    observation.child.terminate()  # Directly created/recorded, now execed fixture.
    assert observation.cleanup(time.monotonic() + CLEANUP), "readiness cleanup unconfirmed"
    final = events(root, root.name)
    assert fixture_finished(root, final) and certify(root, [[root.name, "launch"]] + final, observation, []), "ready lifetime cleanup unconfirmed"
    # Windows cannot end the native fixture from here: it runs to its own finite expiry and writes the marker.
    assert WINDOWS or not any(r[1] == "completed" for r in final), "ready killed fixture unexpectedly wrote a final marker"
    assert not premature and not certified, "missing readiness was certified as cleanup"


def attribution_control():
    try:
        f = dict(copy.deepcopy(COMPANION), observed=False)
        absent = [r for r in f["rows"] if r[1] != "entry"]
        present = sum(r[1] == "entry" for r in f["rows"])
        missing = sum(r[1] == "entry" for r in absent)
        for mode in ("zero", "overflow"):
            arguments = (f["stamped"], f["unchanged"], f["artifacts"], f["stderr"])
            rejected(lambda: validate(mode, f["observed"], missing, *arguments), "OBSERVATION: hook exit and both EOFs were not observed")
            message = "zero budget started context" if mode == "zero" else "unusable numeric budget started context"
            rejected(lambda: validate(mode, f["observed"], present, *arguments), message)
        rejected(lambda: release_valid(timing_control()), "FIXTURE: actual release exhausted the rounded head budget")
    except AssertionError as error:
        raise AssertionError("validator attributed failure to the wrong behavior") from error

def lf_control(work, stage, bash):
    root = work / uuid.uuid4().hex
    root.mkdir()
    home, flat, env = seed(root, stage, "diagnostics", root.name, bash)
    consumer = subprocess.run([bash, "-c", f". {q(flat / 'comm-lib.sh')}; {q(flat / 'comm-context.sh')}"],
                              env=env, capture_output=True, timeout=10)
    native = subprocess.run([sys.executable, "-B", str(Path(__file__).resolve()), "--protocol"],
                            capture_output=True, timeout=10)
    restored = root / "restored.sh"
    executable(restored, "#!/usr/bin/env bash\nprintf 'restored-token\\n'\n")
    restoration = subprocess.run([bash, str(restored)], capture_output=True, timeout=10)
    assert (consumer.returncode == 0 and consumer.stdout == f"NAME={NAME}\n".encode() and
            consumer.stderr == b"completed-diagnostic-token\n" and native.stdout == b"NAME=heartbeat-fixture\n" and
            native.stderr == b"protocol-token\n" and restoration.stdout == b"restored-token\n"), "Bash fixture or protocol did not preserve LF bytes"


def receipt_controls(work):
    root = work / uuid.uuid4().hex
    root.mkdir()
    state = root / "comm/state"
    state.mkdir(parents=True)
    path = state / ".hb-ctx-owned with ' quote"
    path.mkdir()
    receipt = root / "artifact-receipt"
    for content, expected in ((None, "creator artifact receipt missing"), (b"foreign\n/path\n", "creator artifact receipt malformed or foreign"),
                              (f"{root.name}\n{work}\n".encode(), "creator artifact receipt outside owned state"),
                              (f"{root.name}\n{path}\nextra\n".encode(), "creator artifact receipt malformed or foreign")):
        if content is not None:
            receipt.write_bytes(content)
        rejected(lambda: creator_artifact(root, root.name), expected)
        assert path.exists(), "invalid receipt removed owned artifact"
    receipt.write_bytes(f"{root.name}\n{path}\n".encode())
    assert creator_artifact(root, root.name) == path, "creator path identity changed"
    path.rmdir()


def artifact_control(work, stage, bash):
    missed = False
    for mode in ("artifact-create", "artifact-create-stderr"):
        root = work / uuid.uuid4().hex
        root.mkdir()
        home, flat, env = seed(root, stage, mode, root.name, bash)
        before = (home / "registry.json").read_bytes()
        observation = Observation([bash, str(flat / "comm-status-heartbeat.sh")], env, root)
        f = capture_case(root, home, observation, before, mode, mode, "20", 0)
        case_verdict(f)
        assert "heartbeat context artifact could not be created or removed; heartbeat skipped" in f["stderr"], "artifact diagnostic missing"
        path = creator_artifact(root, root.name)
        try:
            remove_creator_artifact(root, root.name, observation.pid)
        except (OSError, AssertionError):
            assert path.exists(), "fixture failed before creator receipt"
            missed = True
            path.rmdir()  # Exact witnessed empty path, after retaining the negative result.
        assert observation.cleanup(time.monotonic() + CLEANUP), "artifact control cleanup unconfirmed"
        assert certify(root, f["rows"], observation, []), "artifact control certification failed"
    assert not missed, "creator-owned artifact was not removed"


def shell_certification_control(work, bash):
    root = work / uuid.uuid4().hex
    root.mkdir()
    source = Path(__file__).resolve().parents[2]
    for name in ("comm", "agents"):
        shutil.copytree(source / name, root / name)
    receipt = root / "owned-root"
    driver = root / "comm/tests/test-heartbeat-ctx-wait.py"
    driver.write_bytes(("import sys\nfrom pathlib import Path\n"
                        f"Path({str(receipt)!r}).write_bytes(sys.argv[1].encode())\n").encode())
    result = subprocess.run([bash, str(root / "comm/tests/test-heartbeat-ctx-wait.sh")],
                            capture_output=True, timeout=30)
    owned = Path(receipt.read_bytes().decode())
    assert owned.is_absolute() and owned.is_dir(), "uncertified shell removed its scratch"
    # No driver children exist; this control's root is now safe to remove.
    shutil.rmtree(owned)
    assert result.returncode != 0 and b"fixture cleanup unconfirmed; retaining scratch" in result.stderr, "uncertified shell entry returned success"


def sensitivity_control(work, bash):
    calls = []
    original = globals()["validate"]
    def recording(*args):
        if args[2] == 0:  # only the corrupted evidence has lost its entry
            calls.append(args)
        return original(*args)
    globals()["validate"] = recording
    stream_original = globals()["stream_assertion"]
    streams = []
    def stream_recording(observation, stream):
        if not isinstance(observation, dict):  # live cases pass frozen dicts; counterexamples pass owners
            streams.append(stream)
        return stream_original(observation, stream)
    globals()["stream_assertion"] = stream_recording
    try:
        sensitivity(work, bash)
    finally:
        globals()["validate"] = original
        globals()["stream_assertion"] = stream_original
    assert calls and all(s in streams for s in ("stdout", "stderr", "exit")), "sensitivity did not reach its real assertion after handshakes"


def harness_controls(work, stage, bash, selected="all"):
    global COMPANION, BUDGET_COMPANION
    if selected in ("all", "H1"):
        lf_control(work, stage, bash)
    if selected in ("all", "H2"):
        receipt_controls(work)
        artifact_control(work, stage, bash)
    if selected in ("all", "H3", "H4", "H5"):
        BUDGET_COMPANION = run_case(work, stage, bash, "control-budget", "release", "10", 0.7)
        assert not BUDGET_COMPANION["failure"], BUDGET_COMPANION["failure"]
    if selected in ("all", "H1", "H4", "H5"):
        COMPANION = run_case(work, stage, bash, "control-success", "diagnostics", "20")
        assert not COMPANION["failure"], COMPANION["failure"]
    if selected in ("all", "H4"):
        attribution_control()
    if selected in ("all", "H5"):
        sensitivity_control(work, bash)
    if selected in ("all", "H6"):
        readiness_control(work, bash)
        shell_certification_control(work, bash)
    print(f"PASS C3-{selected}: portable harness controls")
    if not WINDOWS:
        print("C3-H1/H2/H3: Windows-specific reversal evidence not checked on this OS")


def main():
    global COMPANION, BUDGET_COMPANION
    if len(sys.argv) > 1 and sys.argv[1] == "--protocol":
        protocol(f"NAME={NAME}\n")
        protocol("protocol-token\n", sys.stderr)
        return 0
    if len(sys.argv) > 1 and sys.argv[1] == "--fixture":
        fixture(Path(sys.argv[2]), sys.argv[3], sys.argv[4])
        return 0
    work, stage = map(lambda value: Path(value).resolve(), sys.argv[1:3])
    bash = shutil.which("bash")
    assert bash and shutil.which("jq") and shutil.which("perl"), "FATAL: bash, jq and Perl are required"
    if sys.argv[3:] and sys.argv[3].startswith("--controls"):
        selected = sys.argv[3].partition("=")[2] or "all"
        try:
            harness_controls(work, stage, bash, selected)
            rc = 0
        except AssertionError as error:
            print(f"FAIL C3-{selected}: {error}", flush=True)
            rc = 1
        if certified_observations():
            (work / "cleanup-confirmed").touch()
            print("CLEANUP: confirmed")
        else:
            print("STOP: control owners unconfirmed; retaining scratch")
            rc = 2
        return rc
    sentinel = work / "outside-sentinel"
    sentinel.write_bytes(b"untouched")
    cases = [("R1", "foreground", "20"), ("R2", "foreground" if WINDOWS else "deaf", "20"),
             ("R3", "leader", "20"), ("R4", "setup", "20"), ("diagnostics", "diagnostics", "20"),
             ("B1-0", "zero", "0"), ("B1-000", "zero", "000"),
             ("B2-10", "release", "10", 0.7), ("B2-0010", "release", "0010", 0.7),
             ("B3-21", "release", "21", 1.5), ("B4", "overflow", "999999999999999999999999999999999999")]
    cases += [(mode, mode, "20") for mode in ("success", "nonzero", "exit-one", "repeat", "artifact-create", "artifact-create-stderr", "fallback", "real", "empty-name", "missing", "off", "no-registry", "child-gate", "throttle", "no-row", "absent-floor", "empty-floor")]
    cases += [("budget-" + label, "release", ticks, 1.5) for label, ticks in (("unset", None), ("200", "200"), ("empty", ""), ("invalid", "invalid"))]
    cases += [("budget-" + ticks, "success", ticks) for ticks in ("20", "020", "9223372036854775807")]
    cases += [("deadline-" + label, "default-timeout", ticks) for label, ticks in (("unset", None), ("200", "200"), ("empty", ""), ("invalid", "invalid"))]
    cases += [("P5", "native", "20")] if WINDOWS else [("artifact-remove", "artifact-remove", "20")]
    selected = set(sys.argv[3:])
    if selected:
        assert selected <= {c[0] for c in cases}, "unknown selected case"
        cases = [c for c in cases if c[0] in selected or c[0] in ("success", "B2-10")]
    print("Coverage R2: " + ("Git Bash deferred TERM; ignored TERM not checked on Windows" if WINDOWS else "Unix ignored TERM"))
    print("Coverage budget: " + ("MSYS TERM-responsive Bash" if WINDOWS else "Unix TERM-responsive Bash"))
    print("Coverage " + ("artifact-remove: POSIX directory-mode fault not executed on Windows" if WINDOWS else "P5: Windows-only native case; not executed on this OS"))
    if WINDOWS:
        print("Coverage P5: native Python foreground child with stdout and stderr tokens")
    print("B4 range=0..9223372036854775807 input=999999999999999999999999999999999999; zero/overflow inherit ignored TERM")
    with concurrent.futures.ThreadPoolExecutor(max_workers=1 if WINDOWS else 4) as executor:
        results = list(executor.map(lambda c: run_case(work, stage, bash, *c), cases))
    COMPANION = next(f for f in results if f["label"] == "success")
    BUDGET_COMPANION = next(f for f in results if f["mode"] == "release")
    assert COMPANION["failure"] is None, "successful live companion failed"
    attribution_control()
    sensitivity(work, bash)
    assert sentinel.read_bytes() == b"untouched", "outside scratch sentinel changed"
    assert certified_observations(), "STOP: owners unconfirmed at certification"
    (work / "cleanup-confirmed").touch()
    print("CLEANUP: confirmed")
    (work / "results.json").write_bytes(json.dumps(results, default=str).encode())
    failures = [f for f in results if f["failure"]]
    print(f"heartbeat: {len(results) - len(failures)} passed, {len(failures)} failed; exit/both EOFs/cleanup observed separately")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
