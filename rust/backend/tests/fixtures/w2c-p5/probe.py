"""Measure P5 on an owned bus and inactive units on disposable hosted runners."""

import argparse
import json
import os
from pathlib import Path
import pwd
import re
import signal
import socket
import struct
import subprocess
import tempfile
import time
from xml.sax.saxutils import escape


BASE = "8c5f0d24f733f563cc8b30d292fe37f8d2ab640d"
# Experimental unit input at BASE, not a second production command owner.
COMMAND = (
    "/usr/bin/ssh -T -o BatchMode=yes -o ServerAliveInterval=15 "
    "-o ServerAliveCountMax=3 -o ControlMaster=no -o ControlPath=none "
    "-o ControlPersist=no ${SOT_RELAY_TARGET} ${SOT_RELAY_SOTD} stdio-bridge"
)
MANAGER = "org.freedesktop.systemd1.Manager"
UNIT = "org.freedesktop.systemd1.Unit"
SERVICE = "org.freedesktop.systemd1.Service"
DEST = "org.freedesktop.systemd1"
MANAGER_PATH = "/org/freedesktop/systemd1"


class Receipt:
    def __init__(self, label):
        self.label = label
        self.root = None
        self.private = [pwd.getpwuid(os.getuid()).pw_dir,
                        pwd.getpwuid(os.getuid()).pw_name, socket.gethostname()]
        self.data = {"label": label, "source_commit": BASE, "commands": [],
                     "cases": {}, "checks": [], "status": "NOT CHECKED"}
        # A repository-relative output, not the caller's live home.
        self.output = Path(__file__).resolve().parents[5] / "dev/output/w2c-p5" / label
        self.output.mkdir(parents=True, exist_ok=True)

    def scrub(self, value):
        text = str(value)
        if self.root is not None:
            text = text.replace(str(self.root), "<fixture>")
        for index, item in enumerate(self.private):
            if item:
                replacement = ("<home>", "<user>", "<host>")[index]
                text = re.sub(r"(?<![\w.-])" + re.escape(item) + r"(?![\w.-])",
                              replacement, text)
        text = re.sub(r"(?<!\d)(?:\d{1,3}\.){3}\d{1,3}(?!\d)", "<address>", text)
        return text

    def check(self, name):
        line = f"P5 {self.label} {name} PASS"
        self.data["checks"].append(line)
        print(line, flush=True)

    def save(self):
        self.output.joinpath("receipt.json").write_text(
            self.scrub(json.dumps(self.data, indent=2)) + "\n")


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def hosted_guard(label):
    require(os.environ.get("GITHUB_ACTIONS") == "true" and
            os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted" and
            os.environ.get("GITHUB_REF") == "refs/heads/tmp/ci-w2c-p5",
            "P5 runs only on the dedicated branch on a disposable GitHub-hosted runner")
    require(os.geteuid() != 0, "P5 requires an unprivileged hosted-runner process")
    release = dict(line.split("=", 1) for line in Path("/etc/os-release").read_text().splitlines()
                   if "=" in line)
    wanted = "22.04" if label == "systemd-249" else "24.04"
    require(release.get("ID", "").strip('"') == "ubuntu" and
            release.get("VERSION_ID", "").strip('"') == wanted,
            "P5 runner image does not match the requested label")


class Fixture:
    def __init__(self, root, receipt):
        self.root = root
        self.receipt = receipt
        self.children = []
        for folder in ("home", "config", "data", "state", "runtime", "units"):
            root.joinpath(folder).mkdir(mode=0o700)
        self.runtime = root / "runtime"
        self.address = f"unix:path={self.runtime}/bus"
        # No inherited bus, agent identity, systemd settings, or service-activation directories.
        self.env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "LANG": "C",
                    "LC_ALL": "C", "HOME": str(root / "home"),
                    "XDG_CONFIG_HOME": str(root / "config"),
                    "XDG_DATA_HOME": str(root / "data"),
                    "XDG_STATE_HOME": str(root / "state"),
                    "XDG_RUNTIME_DIR": str(self.runtime),
                    "DBUS_SESSION_BUS_ADDRESS": self.address,
                    "SYSTEMD_UNIT_PATH": str(root / "units"),
                    "SYSTEMD_LOG_TARGET": "console", "SYSTEMD_LOG_LEVEL": "warning"}
        self.manager = None

    def run(self, args, name, expected=0):
        start = time.monotonic()
        try:
            result = subprocess.run(args, env=self.env, stdin=subprocess.DEVNULL,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10)
        except subprocess.TimeoutExpired as error:
            self.receipt.data["commands"].append(
                {"name": name, "argv": args, "wrapper_timeout": True,
                 "stdout": (error.stdout or b"").decode("utf-8", "backslashreplace"),
                 "stderr": (error.stderr or b"").decode("utf-8", "backslashreplace")})
            raise RuntimeError(f"{name}: external hang guard fired; busctl bound not established")
        output = result.stdout.decode("utf-8", "backslashreplace")
        errors = result.stderr.decode("utf-8", "backslashreplace")
        self.receipt.data["commands"].append(
            {"name": name, "argv": args, "exit": result.returncode,
             "elapsed_seconds": time.monotonic() - start, "stdout": output, "stderr": errors})
        if expected is not None:
            require(result.returncode == expected, f"{name}: exit {result.returncode}: {errors}")
        return result.returncode, output, errors

    def bus(self, args, name, expected=0, bound="2s"):
        return self.run(["/usr/bin/busctl", "--user", f"--address={self.address}",
                         "--json=short", f"--timeout={bound}", *args], name, expected)

    def call(self, method, unit, name, expected=0, bound="2s"):
        return self.bus(["call", DEST, MANAGER_PATH, MANAGER, method, "s", unit],
                        name, expected, bound)

    def property(self, path, interface, prop, name, expected=0, bound="2s"):
        return self.bus(["get-property", DEST, path, interface, prop], name, expected, bound)

    def spawn(self, args, tag):
        log = self.root / f"{tag}.log"
        with log.open("wb") as output:
            child = subprocess.Popen(args, env=self.env, stdin=subprocess.DEVNULL,
                                     stdout=output, stderr=subprocess.STDOUT)
        self.children.append((tag, child, log))  # Retain only children we created.
        self.receipt.data.setdefault("owned_children", []).append({"role": tag, "pid": child.pid})
        return child

    def start(self):
        config = self.root / "bus.conf"
        config.write_text("<busconfig><type>session</type><listen>" + escape(self.address) +
                          "</listen><auth>EXTERNAL</auth><policy context=\"default\">"
                          "<allow send_destination=\"*\"/><allow receive_sender=\"*\"/>"
                          "<allow own=\"*\"/></policy></busconfig>\n")
        self.root.joinpath("units/default.target").write_text(
            "[Unit]\nDescription=Inactive P5 fixture target\nDefaultDependencies=no\n")
        bus = self.spawn(["/usr/bin/dbus-daemon", "--nofork", "--nopidfile",
                          f"--config-file={config}"], "bus")
        deadline = time.monotonic() + 30
        while not self.runtime.joinpath("bus").exists():
            require(bus.poll() is None, "private bus exited before readiness")
            require(time.monotonic() < deadline, "private bus did not become ready")
            time.sleep(0.05)
        manager_bin = "/usr/lib/systemd/systemd"
        if not Path(manager_bin).is_file():
            manager_bin = "/lib/systemd/systemd"
        _, version, _ = self.run([manager_bin, "--version"], "manager-binary-version")
        self.receipt.data["manager_binary_version"] = version
        if self.receipt.label == "systemd-249":
            require(re.match(r"systemd 249(?:\D|$)", version), "manager binary is not systemd 249")
        self.manager = self.spawn([manager_bin, "--user", "--unit=default.target"], "manager")
        while True:
            require(self.manager.poll() is None, "private manager exited before readiness")
            rc, output, _ = self.bus(
                ["call", "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                 "GetConnectionUnixProcessID", "s", DEST], "manager-owner", None)
            if rc == 0:
                require(scalar(output, "u") == self.manager.pid,
                        "private bus's systemd owner is not our recorded child")
                break
            require(time.monotonic() < deadline, "private manager did not acquire its bus name")
            time.sleep(0.05)
        # systemctl may use the direct manager socket; prove that endpoint is ours too.
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as peer:
            peer.settimeout(2)
            peer.connect(str(self.runtime / "systemd/private"))
            pid, uid, _ = struct.unpack("3i", peer.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
            require(pid == self.manager.pid and uid == os.getuid(), "foreign private manager endpoint")
        self.receipt.check("private-bus-and-manager-owned")

    def close(self):
        failures = []
        for tag, child, log in reversed(self.children):
            try:
                if child.poll() is None:
                    child.send_signal(signal.SIGCONT)
                    child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=5)
                self.receipt.data.setdefault("cleanup", []).append({"role": tag, "exit": child.returncode})
            except Exception as error:
                failures.append(f"{tag}: {type(error).__name__}: {error}")
            self.receipt.data.setdefault("process_logs", {})[tag] = log.read_text(errors="backslashreplace")
        require(not failures, "owned-child cleanup failed: " + "; ".join(failures))
        self.receipt.check("owned-children-reaped")


def scalar(output, signature):
    envelope = json.loads(output)
    require(envelope.get("type") == signature, f"expected {signature} response, got {envelope}")
    value = envelope.get("data")
    while isinstance(value, list) and len(value) == 1:
        value = value[0]
    require(isinstance(value, str) if signature in ("o", "s") else type(value) is int,
            "scalar payload is missing or has another representation")
    return value


def commands(output):
    # Save the actual envelope before using a structural extractor: nesting is a measured result.
    envelope = json.loads(output)
    require(envelope.get("type") == "a(sasbttttuii)", "unexpected ExecStart D-Bus signature")
    found = []

    def visit(value, path):
        if isinstance(value, list):
            if (len(value) == 10 and isinstance(value[0], str) and isinstance(value[1], list)
                    and all(isinstance(arg, str) for arg in value[1]) and type(value[2]) is bool
                    and all(type(item) is int for item in value[3:])):
                found.append({"json_path": path, "tuple": value})
            else:
                for index, item in enumerate(value):
                    visit(item, f"{path}[{index}]")
    require("data" in envelope, "ExecStart envelope has no data")
    visit(envelope["data"], "data")
    return envelope, found


def old_accept(display):
    lines = display.splitlines()
    execs = [line for line in lines if line.startswith("ExecStart=")]
    return (len(execs) == 1 and f"argv[]={COMMAND} ;" in execs[0]
            and any(line.rstrip() == "LoadState=loaded" for line in lines))


def write_cases(fixture):
    cases = {
        "generated": ("simple", COMMAND),
        "alternate-executable": ("simple", "@/usr/bin/true " + COMMAND),
        "ignore-errors": ("simple", "-" + COMMAND),
        "merged-argument": ("simple", COMMAND.replace(
            "${SOT_RELAY_SOTD} stdio-bridge", '"${SOT_RELAY_SOTD} stdio-bridge"')),
        "changed-command": ("simple", COMMAND + " --label local"),
        "extra-command": ("oneshot", COMMAND + "\nExecStart=/usr/bin/true"),
        "bad-setting": ("simple", COMMAND + "\nExecStart=/usr/bin/true"),
    }
    for name, (kind, command) in cases.items():
        fixture.root.joinpath(f"units/sot-host-relay-{name}@.service").write_text(
            "[Unit]\nDescription=Inactive P5 relay fixture\nCollectMode=inactive-or-failed\n"
            "[Service]\n" + f"Type={kind}\nStandardInput=socket\nStandardOutput=socket\n"
            "StandardError=journal\nEnvironment=SOT_RELAY_TARGET=fixture-target\n"
            "Environment=SOT_RELAY_SOTD=sotd\n"
            f"ExecStartPre=/usr/bin/touch {fixture.root}/executed\nExecStart={command}\n")
    return cases


def measure_case(fixture, name):
    unit = f"sot-host-relay-{name}@refresh-check.service"
    _, load, _ = fixture.call("LoadUnit", unit, f"{name}-LoadUnit")
    path = scalar(load, "o")
    _, get, _ = fixture.call("GetUnit", unit, f"{name}-GetUnit")
    require(scalar(get, "o") == path, f"{name}: GetUnit differs from LoadUnit")
    _, before, _ = fixture.property(path, UNIT, "ActiveState", f"{name}-ActiveState-before")
    _, state, _ = fixture.property(path, UNIT, "LoadState", f"{name}-LoadState")
    _, execution, _ = fixture.property(path, SERVICE, "ExecStart", f"{name}-ExecStart")
    _, display, _ = fixture.run(
        ["/usr/bin/systemctl", "--user", "--no-pager", "show", "-p", "ExecStart", "-p", "LoadState", unit],
        f"{name}-old-display")
    _, after, _ = fixture.property(path, UNIT, "ActiveState", f"{name}-ActiveState-after")
    envelope, records = commands(execution)
    observed = {"LoadUnit": json.loads(load), "GetUnit": json.loads(get),
                "LoadState": json.loads(state), "ActiveState_before": json.loads(before),
                "ActiveState_after": json.loads(after), "ExecStart": envelope,
                "command_records": records, "old_display": display, "old_accept": old_accept(display)}
    fixture.receipt.data["cases"][name] = observed
    require(scalar(before, "s") == scalar(after, "s") == "inactive", f"{name}: unit activated")
    require(not fixture.root.joinpath("executed").exists(), f"{name}: execution marker exists")
    if name == "bad-setting":
        require(scalar(state, "s") == "bad-setting" and not observed["old_accept"],
                "invalid multiple-command control did not fail load")
    else:
        require(scalar(state, "s") == "loaded", f"{name}: not a loadable identity fixture")
        require(len(records) == (2 if name == "extra-command" else 1), f"{name}: wrong command count")
        record = records[0]["tuple"]
        expected_argv = COMMAND.split()
        if name == "merged-argument":
            expected_argv[-2:] = ["${SOT_RELAY_SOTD} stdio-bridge"]
        elif name == "changed-command":
            expected_argv += ["--label", "local"]
        require(record[1] == expected_argv, f"{name}: arguments expanded or represented differently")
        require(record[0] == ("/usr/bin/true" if name == "alternate-executable" else "/usr/bin/ssh"),
                f"{name}: executable differs from experimental input")
        require(record[2] == (name == "ignore-errors"), f"{name}: execution policy differs")
        if name in ("generated", "alternate-executable", "ignore-errors"):
            require(observed["old_accept"], f"{name}: expected old-verifier acceptance was not measured")
        elif name != "merged-argument":
            require(not observed["old_accept"], f"{name}: old rejection control accepted")
    fixture.receipt.check(f"{name} inactive-and-identity-measured")
    return unit, path


def timeout_probes(fixture, unit, path):
    manager = fixture.manager
    require(manager.poll() is None, "private manager exited before timeout controls")
    manager.send_signal(signal.SIGSTOP)
    try:
        deadline = time.monotonic() + 5
        while True:
            observation = os.waitid(os.P_PID, manager.pid, os.WSTOPPED | os.WNOWAIT | os.WNOHANG)
            if observation and observation.si_code == os.CLD_STOPPED:
                break
            require(time.monotonic() < deadline, "our manager did not report stopped")
            time.sleep(0.05)
        probes = [
            ("LoadUnit", lambda: fixture.call("LoadUnit", unit, "timeout-LoadUnit", None, "250ms")),
            ("GetUnit", lambda: fixture.call("GetUnit", unit, "timeout-GetUnit", None, "250ms")),
            ("LoadState", lambda: fixture.property(path, UNIT, "LoadState", "timeout-LoadState", None, "250ms")),
            ("ExecStart", lambda: fixture.property(path, SERVICE, "ExecStart", "timeout-ExecStart", None, "250ms")),
        ]
        for name, probe in probes:
            rc, _, errors = probe()
            require(rc != 0 and "timed out" in errors.lower(), f"{name}: no named busctl timeout")
            fixture.receipt.check(f"timeout-{name} bounded")
    finally:
        if manager.poll() is None:
            manager.send_signal(signal.SIGCONT)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--label", choices=("systemd-249", "systemd-current"), required=True)
    args = parser.parse_args()
    # Guard before any fixture home, bus, manager, receipt directory, or subprocess is created.
    try:
        hosted_guard(args.label)
    except RuntimeError as error:
        print(f"P5 {args.label} NOT CHECKED: {error}", flush=True)
        return 1
    receipt = Receipt(args.label)
    try:
        with tempfile.TemporaryDirectory(prefix="w2c-p5-", dir="/tmp") as directory:
            root = Path(directory).resolve()
            receipt.root = root
            fixture = Fixture(root, receipt)
            try:
                _, version, _ = fixture.run(["/usr/bin/busctl", "--version"], "busctl-version")
                _, manager_version, _ = fixture.run(["/usr/bin/systemctl", "--version"], "systemd-version")
                receipt.data["busctl_version"] = version
                receipt.data["systemd_version"] = manager_version
                if args.label == "systemd-249":
                    require(re.match(r"systemd 249(?:\D|$)", version) and
                            re.match(r"systemd 249(?:\D|$)", manager_version), "minimum is not systemd 249")
                receipt.check("versions-recorded")
                cases = write_cases(fixture)
                fixture.start()
                generated = None
                for name in cases:
                    result = measure_case(fixture, name)
                    if name == "generated":
                        generated = result
                timeout_probes(fixture, *generated)
                require(not root.joinpath("executed").exists(), "an inspection activated a service")
                receipt.check("no-service-executed")
            finally:
                fixture.close()
        receipt.data["status"] = "PASS"
        receipt.check("COMPLETE")
        return 0
    except Exception as error:
        receipt.data["status"] = "FAIL"
        receipt.data["error"] = receipt.scrub(f"{type(error).__name__}: {error}")
        print(f"P5 {args.label} FAIL {receipt.data['error']}", flush=True)
        return 1
    finally:
        receipt.save()


if __name__ == "__main__":
    raise SystemExit(main())
