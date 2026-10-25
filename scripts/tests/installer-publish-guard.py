#!/usr/bin/env python3
"""Real sotd install orchestration in scratch profiles, with actual Git-launched pushes.

Run after building the three binaries, on Linux/macOS/Windows. --scenario repair is also a baseline witness against
4464089f7's binaries and checkout: successful orchestration there leaves both known clones unenrolled. Unix Julia steps
and the Windows shortcut are recording fixtures. Windows leaves Julia unavailable and checks its human-step verdict;
its unique scratch USERNAME isolates the daemon pipe. No row, service, window, agent or network request is started.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time

# Importing the helper must not write a bytecode cache into the supplied source checkout.
sys.dont_write_bytecode = True
from installer_process import RUNNER_ENV, capture, descendants


def run(argv, cwd, env, ok=True, input_text=None, *, step=None, timeout=180):
    def report_timeout(pid, error):
        # TimeoutExpired can carry bytes even with text=True. Keep the installer's last progress lines: the command
        # alone cannot distinguish enrollment, layout, a daemon query, or a converge waiting on a missing fixture.
        output = error.stdout or ""
        if isinstance(output, bytes):
            output = output.decode("utf-8", errors="replace")
        snapshot = descendants(pid, cwd)
        print(f"{step or argv[0]}: timed out after {timeout} s\n{argv}\n"
              f"captured output before termination:\n{output}\n"
              f"process root {pid} and descendants before termination (listing bound 10 s):\n{snapshot}",
              file=sys.stderr, flush=True)

    try:
        result = capture(argv, cwd, env, input_text, timeout=timeout, on_timeout=report_timeout)
    except subprocess.TimeoutExpired:
        raise AssertionError(f"{step or argv[0]}: timed out after {timeout} s; output and descendants printed before tree cleanup") from None
    if ok and result.returncode:
        raise AssertionError(f"{argv}: exit {result.returncode}\n{result.stdout}")
    return result


def refused_with(result, expected_line):
    guard_lines = [line for line in result.stdout.splitlines() if line.startswith("pre-push: ")]
    return result.returncode == 1 and guard_lines == [expected_line]


def windows_diagnostic_matrix(root, base, system):
    """Experiments only: none of these environment variants is ever used to install."""
    executable = system / "WindowsPowerShell/v1.0/powershell.exe"
    exact = "@(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Select-Object ProcessId,ParentProcessId,Name,ExecutablePath,CommandLine) | ConvertTo-Json -Compress"
    phases = r'''
$ErrorActionPreference = 'Stop'
function Phase([string]$Name) { [Console]::Out.WriteLine('PGB_PHASE=' + $Name); [Console]::Out.Flush() }
Phase 'script-start'
'''
    staged = r'''
Phase 'before-imports'
Import-Module CimCmdlets,Microsoft.PowerShell.Utility -ErrorAction Stop
Phase 'after-imports'
Phase 'before-enumeration'
$items = @(Get-CimInstance Win32_Process -ErrorAction Stop)
Phase 'after-enumeration'
$rows = @($items | Select-Object ProcessId,ParentProcessId,Name,ExecutablePath,CommandLine)
Phase 'after-projection'
$json = $rows | ConvertTo-Json -Compress
Phase 'after-json'
$decoded = ConvertFrom-Json -InputObject $json
$own = @($decoded | Where-Object { $_.ProcessId -eq $PID })
if ($own.Count -ne 1 -or [string]::IsNullOrEmpty($own[0].CommandLine)) { throw 'CIM did not observe its own process' }
Phase 'after-self-validation'
'''
    def fresh(label):
        folder = root / ("matrix-" + label)
        for name in ["home", "local", "config", "run", "tmp"]:
            (folder / name).mkdir(parents=True)
        env = dict(base)
        env.update(HOME=str(folder / "home"), USERPROFILE=str(folder / "home"), LOCALAPPDATA=str(folder / "local"),
                   APPDATA=str(folder / "config"), XDG_CONFIG_HOME=str(folder / "config"), XDG_STATE_HOME=str(folder / "state"),
                   XDG_RUNTIME_DIR=str(folder / "run"), TMPDIR=str(folder / "tmp"), TMP=str(folder / "tmp"), TEMP=str(folder / "tmp"),
                   SOT_COMM_HOME=str(folder / "home/comm"), CODEX_HOME=str(folder / "home/codex"),
                   CLAUDE_CONFIG_DIR=str(folder / "home/claude"))
        return env

    def variant(label, env, body=exact, bound=15):
        start = time.monotonic()
        output = ""
        print(f"CIM matrix start: {label}; bound={bound}s", flush=True)
        try:
            result = capture([executable, "-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
                              phases + body + "\nPhase 'complete'\n"], root, env, timeout=bound)
            output, status = result.stdout, f"exit={result.returncode}"
        except subprocess.TimeoutExpired as error:
            output, status = error.stdout or "", "timeout"
        if isinstance(output, bytes):
            output = output.decode("utf-8", errors="replace")
        markers = [line.removeprefix("PGB_PHASE=") for line in output.splitlines() if line.startswith("PGB_PHASE=")]
        phase = markers[-1] if markers else "before-script"
        print(f"CIM matrix: {label}; elapsed={time.monotonic() - start:.3f}s; {status}; last-phase={phase}; bound={bound}s", flush=True)
        return status == "exit=0"

    variant("current .NET-only startup", base, "[Console]::Out.WriteLine('dotnet-only'); [Console]::Out.Flush()")
    variant("current exact product query", base)
    variant("current staged query", base, staged)
    variant("repeat current after staged query", base)
    cold = fresh("cold")
    completed = variant("fresh cold exact query", cold, bound=45)
    variant("same profile repeat exact query", cold)
    print(f"CIM matrix: cold-completed={completed}; a timed-out cold attempt does not establish completed warming", flush=True)
    no_cache = fresh("no-cache")
    no_cache["PSModuleAnalysisCachePath"] = "nul"
    variant("fresh module analysis cache disabled", no_cache)
    variant("repeat current after cache experiment", base)
    imports = r'''
Phase 'before-absolute-imports'
$modules = [IO.Path]::Combine($env:SystemRoot, 'System32\WindowsPowerShell\v1.0\Modules')
Import-Module ([IO.Path]::Combine($modules, 'CimCmdlets\CimCmdlets.psd1')) -ErrorAction Stop
Import-Module ([IO.Path]::Combine($modules, 'Microsoft.PowerShell.Utility\Microsoft.PowerShell.Utility.psd1')) -ErrorAction Stop
Phase 'after-absolute-imports'
'''
    variant("fresh absolute system-module imports", fresh("imports"), imports + exact)
    variant("only USERNAME restored", dict(base, USERNAME=RUNNER_ENV["USERNAME"]))
    variant("only PATH restored", dict(base, PATH=RUNNER_ENV["PATH"]))
    temporary = root / "matrix-native-temp"
    temporary.mkdir()
    probe = temporary / "writable"
    probe.write_bytes(b"scratch")
    assert probe.read_bytes() == b"scratch"
    probe.unlink()
    variant("only TEMP and TMP changed", dict(base, TEMP=str(temporary), TMP=str(temporary)))
    isolated = fresh("near-full")
    near_full = {k.upper(): v for k, v in RUNNER_ENV.items() if not k.upper().startswith(("SOT_", "CODEX_", "CLAUDE_", "JULIA_"))}
    for key, value in isolated.items():
        if key != "PATH":
            near_full[key.upper()] = value
    near_full["PSMODULEANALYSISCACHEPATH"] = str(Path(isolated["LOCALAPPDATA"]) / "module-analysis-cache")
    near_full["HOMEDRIVE"] = Path(isolated["USERPROFILE"]).drive
    near_full["HOMEPATH"] = str(Path(isolated["USERPROFILE"]))[len(near_full["HOMEDRIVE"]):]
    variant("near-full variables with scratch profile and cache", near_full)
    variant("repeat current after environment experiments", base)
    projected = "@(Get-CimInstance Win32_Process -Property ProcessId,ParentProcessId,CreationDate,CommandLine -OperationTimeoutSec 5 | Select-Object ProcessId,ParentProcessId,CreationDate,CommandLine) | ConvertTo-Json -Compress"
    variant("current projected properties and operation timeout", base, projected)
    print("CIM matrix complete: diagnostic variants do not select or modify the install environment", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--sotd", type=Path, required=True)
    parser.add_argument("--sot", type=Path, required=True)
    parser.add_argument("--capsule", type=Path, required=True)
    parser.add_argument("--scenario", choices=["fresh", "repair"], default="repair")
    parser.add_argument("--windows-diagnostics-only", action="store_true", help="run the bounded diagnostic matrix once, without installing")
    parser.add_argument("--case", choices=["all", "enrollment", "migration", "policy", "refusal"], default="all")
    args = parser.parse_args()
    binaries = {"sotd": args.sotd.resolve(), "sot": args.sot.resolve(), "sot-capsule": args.capsule.resolve()}
    windows = os.name == "nt"
    suffix = ".exe" if windows else ""
    git = shutil.which("git.exe" if windows else "git")
    if not git:
        raise RuntimeError("Git is required; this case cannot be skipped")
    git = Path(git).resolve()
    with tempfile.TemporaryDirectory(prefix="sot-installer-guard-") as scratch:
        root = Path(scratch).resolve()
        home, tools, prefix, pack = (root / name for name in ["home", "tools", "prefix", "pack"])
        for folder in [home, tools, pack, root / "config", root / "local", root / "run", root / "tmp"]:
            folder.mkdir()
        if not windows:
            (root / "run").chmod(0o700)
        for name in ["global", "system"]:
            (root / name).write_text("", encoding="utf-8")
        (root / "global").write_text("[gc]\n auto = 0\n[maintenance]\n auto = false\n", encoding="utf-8")
        env = {k: v for k, v in os.environ.items() if k.upper() in {
            "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "PROCESSOR_ARCHITECTURE", "PROCESSOR_ARCHITEW6432"}}
        env.update(HOME=str(home), USERPROFILE=str(home), LOCALAPPDATA=str(root / "local"), APPDATA=str(root / "config"),
                   XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"), XDG_RUNTIME_DIR=str(root / "run"),
                   TMPDIR=str(root / "tmp"), TMP=str(root / "tmp"), TEMP=str(root / "tmp"),
                   SOT_COMM_HOME=str(home / ".sot-comm"), CODEX_HOME=str(home / ".codex"),
                   CLAUDE_CONFIG_DIR=str(home / ".claude"), SOT_SELF_HOST="fixture", LC_ALL="C",
                   GIT_CONFIG_GLOBAL=str(root / "global"), GIT_CONFIG_SYSTEM=str(root / "system"), GIT_CONFIG_NOSYSTEM="1",
                   GIT_TERMINAL_PROMPT="0", GIT_ALLOW_PROTOCOL="file",
                   GIT_AUTHOR_NAME="Fixture", GIT_AUTHOR_EMAIL="fixture@example.invalid",
                   GIT_COMMITTER_NAME="Fixture", GIT_COMMITTER_EMAIL="fixture@example.invalid",
                   GIT_AUTHOR_DATE="2020-01-01T00:00:00Z", GIT_COMMITTER_DATE="2020-01-01T00:00:00Z")
        if windows:
            # The Windows endpoint uses USERNAME, not USERPROFILE or LOCALAPPDATA. Without it, every stripped
            # environment probes the shared sot-user-local fallback rather than a fixture-owned daemon namespace.
            env["USERNAME"] = "sot-scratch-" + root.name
            machine_keys = {
                "PROGRAMFILES", "PROGRAMFILES(X86)", "PROGRAMW6432",  # PowerShell/.NET machine installation roots.
                "PROGRAMDATA", "ALLUSERSPROFILE",  # Shared Windows data roots, not the account's profile.
                "SYSTEMDRIVE",  # Drive for Windows/.NET default machine paths.
                "COMPUTERNAME",  # CIM's local machine identity; SOT_SELF_HOST remains synthetic.
            }
            env.update({k: v for k, v in os.environ.items() if k.upper() in machine_keys})
            jq = shutil.which("jq.exe")
            if not jq:
                raise RuntimeError("native jq.exe is a fixture prerequisite")
            shutil.copy2(jq, tools / "jq.exe")
            system = Path(env["SYSTEMROOT"]) / "System32"
            # Retain the OS module root explicitly, including CimCmdlets. Never inherit PSModulePath's user module
            # folders or let PowerShell rebuild it from the runner's known profile folders.
            env["PSModulePath"] = str(system / "WindowsPowerShell/v1.0/Modules")
            # Keep native Git in its installation so the installer can find that installation's Bash.
            path = [tools, git.parent, system, system / "WindowsPowerShell/v1.0", system / "Wbem"]
        else:
            # No installed Julia, npm, service tool or agent can be reached. Git still finds its own libexec tools.
            for name in ["bash", "sh", "cat", "chmod", "cmp", "cp", "git", "grep", "ldd", "mkdir", "mktemp", "mv", "od", "rm", "sed", "stat", "tr", "uname", "awk", "id"]:
                executable = shutil.which(name)
                if executable:
                    (tools / name).symlink_to(executable)
            for name in ["ps", "jq"]:
                (tools / name).write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
                (tools / name).chmod(0o755)
            path = [tools]
        env["PATH"] = os.pathsep.join(str(p) for p in path)
        if args.windows_diagnostics_only:
            if windows:
                windows_diagnostic_matrix(root, env, system)
            else:
                print("SKIP: CIM diagnostic matrix requires native Windows", flush=True)
            return

        def g(cwd, *words, ok=True, data=None):
            return run([git, "-C", cwd, *words], cwd=root, env=env, ok=ok, input_text=data)

        version_line = run([binaries["sotd"], "--version"], root, env).stdout
        if windows:
            pipe = run([binaries["sotd"], "session-socket-path", "local"], root, env,
                       step="Windows scratch pipe derivation", timeout=15).stdout.strip()
            assert pipe == "\\\\.\\pipe\\sot-" + env["USERNAME"] + "-local", pipe
            # Enumerate names without opening a pipe instance, as the installer's read-only probe does.
            assert pipe.rsplit("\\", 1)[1].casefold() not in {name.casefold() for name in os.listdir("\\\\.\\pipe\\")}, (
                "Windows scratch daemon pipe already exists")
            absent = run([binaries["sotd"], "same-version", "--endpoint", "pipe:" + pipe], root, env, ok=False,
                         step="Windows scratch daemon absence check", timeout=15)
            assert absent.returncode == 2, f"scratch pipe must have no daemon: exit {absent.returncode}\n{absent.stdout}"
            assert shutil.which("julia", path=env["PATH"]) is None, "Windows fixture must leave Julia as a named install step"
        version = version_line.split()[1]
        match = re.search(r"\(([0-9a-f]{7,40}) ", version_line)
        if not match:
            raise AssertionError(f"the real sotd must name its build commit: {version_line}")
        commit = g(args.repo.resolve(), "rev-parse", match[1]).stdout.strip()
        # All later Git writes target fixture-owned clones; the lane checkout is only this clone's source.
        origin = root / "origin.git"
        g(root, "clone", "--bare", "--no-hardlinks", args.repo.resolve(), origin)
        env["SOT_INSTALL_ORIGIN"] = str(origin)
        base, checkout, caller = prefix / "repo/base", prefix / "repo/versions" / ("v" + version), root / "caller with spaces"
        base.parent.mkdir(parents=True)
        g(root, "clone", "--no-checkout", origin, base)
        g(base, "worktree", "add", "--detach", checkout, commit)
        g(root, "clone", origin, caller)
        g(caller, "checkout", "--detach", commit)
        # The installer exercises a real checkout, but no Windows shortcut/pinning API is part of this witness.
        if windows:
            (checkout / "scripts/install-shortcut.ps1").write_text(
                "param([string]$Prefix, [switch]$NoManifest)\n"
                "[IO.File]::WriteAllText((Join-Path $Prefix 'shortcut-called'), 'called')\nexit 0\n", encoding="ascii")
        else:
            julia = tools / "julia"
            julia.write_text(
                "#!/bin/sh\ncase \"$*\" in *'print(VERSION)'*) printf 1.12.0; exit 0 ;; esac\n"
                "project=\nfor a do case \"$a\" in --project=*) project=${a#--project=} ;; esac; done\n"
                "[ -n \"$project\" ] || exit 0\n"
                "case \"$*\" in *ShipTools.update_comm*) mkdir -p \"$SOT_COMM_HOME\"; printf '%s\\n' " + shlex.quote(commit[:9]) +
                " > \"$SOT_COMM_HOME/VERSION\" ;; *) mkdir -p \"$project\"; : > \"$project/Manifest.toml\" ;; esac\n",
                encoding="utf-8")
            julia.chmod(0o755)
        target = "windows-x86_64" if windows else ("macos-aarch64" if os.uname().sysname == "Darwin" else "linux-x86_64")
        top = pack / f"sot-{version}-{target}"
        top.mkdir()
        for name, binary in binaries.items():
            shutil.copy2(binary, top / (name + suffix))
        archive = f"sot-{version}-{target}" + (".zip" if windows else ".tar.gz")
        (pack / archive).write_bytes(b"synthetic archive: body fixture, no bootstrap unpack\n")
        (pack / "COMMIT").write_text(commit + "\n", encoding="ascii")
        (pack / "SHA256SUMS").write_text("".join(
            hashlib.sha256((pack / name).read_bytes()).hexdigest() + "  " + name + "\n"
            for name in [archive, "COMMIT"]), encoding="ascii")
        if args.scenario == "repair":
            (prefix / "bin").mkdir()
            for name, binary in binaries.items():
                shutil.copy2(binary, prefix / "bin" / (name + suffix))
            config_record = root / "config/sot"  # Windows record retains the pre-existing APPDATA spelling.
            record = dict(schema=1, prefix=str(prefix), config=str(config_record), service="none", version=version,
                          tag="v" + version, commit=commit, installed_at="2020-01-01T00:00:00Z", hub="",
                          daemon=True, frontend=windows, pinned=True)
            (prefix / "install.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
        if windows:
            (root / "local/sot/config").mkdir(parents=True)
            policy = root / "local/sot/config/publish-list.txt"
        else:
            (root / "config/sot").mkdir(parents=True)
            policy = root / "config/sot/publish-list.txt"
        denied_name = "fixturedeny" + root.name.rsplit("-", 1)[-1]
        policy.write_text("# synthetic policy\n" + denied_name + "\n", encoding="utf-8")
        remote = root / "remote.git"
        g(root, "init", "--bare", remote)
        command = [binaries["sotd"], "install", "--pack", pack]
        command += ["-Archive", pack, "-Prefix", prefix] if windows else ["--archive", pack, "--prefix", prefix, "--be-only", "--no-service"]

        def install(cwd=caller, success=True):
            result = run(command, cwd, env, ok=success, step=f"{args.scenario} installer publish enrollment ({args.case})")
            lines = [line for line in result.stdout.splitlines() if re.match(r"^(PASS|FAIL|ONE STEP LEFT): ", line)]
            assert len(lines) == 1 and result.stdout.rstrip().endswith(lines[0]), result.stdout
            if success:
                assert not lines[0].startswith("FAIL:"), result.stdout
                if args.scenario == "repair":
                    assert "a repair run" in result.stdout, result.stdout
                if windows:
                    assert "install Julia" in lines[0], result.stdout
                    assert (prefix / "shortcut-called").read_text(encoding="ascii") == "called", result.stdout
            return result, lines[0]

        def later_refusal():
            # A shared install belongs to its declared daemon host. This gate follows recognized-caller discovery;
            # the pack's scratch may be created, but neither clone nor protected installed state may change.
            record_path = prefix / "install.json"
            hosts = root / "config/sot/hosts.toml"
            pending = prefix / "updates" / ("pending-" + target + ".json")
            saved = {p: p.read_bytes() if p.exists() else None for p in [record_path, hosts, pending]}
            updates_existed = pending.parent.exists()
            shared = dict(schema=1, version=version, commit=commit, daemon=True, frontend=False, service="systemd", hub="")
            record_path.write_text(json.dumps(shared, indent=2) + "\n", encoding="ascii")
            hosts.write_text('hub = "hub-box"\n[host.hub-box]\ndaemon = true\n', encoding="ascii")
            pending.parent.mkdir(exist_ok=True)
            pending.write_text('{"tag":"v9"}\n', encoding="ascii")
            protected = [prefix / name for name in ["install.json", "bin", "updates", "repo/current"]]
            protected += [home / name for name in [".local/bin", ".config/systemd", ".local/share/applications", "Applications", ".sot-comm"]]
            protected += [root / "config/sot", root / "config/systemd"]
            for clone in [base, caller]:
                protected += [clone / ".git" / name for name in ["config", "config.worktree", "hooks", "publish-guard"]]

            def snapshot():
                state = {}
                def visit(path):
                    if path.is_symlink():
                        state[str(path)] = ("link", os.readlink(path))
                    elif path.is_file():
                        digest = hashlib.sha256()
                        with path.open("rb") as source:
                            for chunk in iter(lambda: source.read(65536), b""):
                                digest.update(chunk)
                        state[str(path)] = ("file", path.stat().st_mode, digest.hexdigest())
                    elif path.is_dir():
                        state[str(path)] = ("directory", path.stat().st_mode)
                        for child in sorted(path.iterdir()):
                            visit(child)
                    else:
                        state[str(path)] = ("missing",)
                for path in protected:
                    visit(path)
                return state

            probe_tools = root / "discovery-tools"
            probe_tools.mkdir()
            probe_git = probe_tools / "git"
            probe_git.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$SOT_PGB_DISCOVERY_LOG"\nexec "$SOT_PGB_REAL_GIT" "$@"\n', encoding="ascii")
            probe_git.chmod(0o755)
            log = root / "discovery.log"
            refusal_env = dict(env, PATH=str(probe_tools) + os.pathsep + env["PATH"],
                               SOT_PGB_REAL_GIT=str(git), SOT_PGB_DISCOVERY_LOG=str(log))
            before = snapshot()
            rejected = run(command, caller, refusal_env, ok=False, step="later_role_refusal_preserves_recognized_clones_and_install")
            after = snapshot()
            expected = f"FAIL: gate: hosts.toml runs no sotd on this host, and {prefix} already holds an install"
            verdicts = [line for line in rejected.stdout.splitlines() if line.startswith("FAIL:")]
            assert rejected.returncode == 2 and verdicts == [expected], rejected.stdout
            calls = log.read_text(encoding="utf-8").splitlines()
            for operation in ["--show-toplevel", "remote.origin.url", "--git-common-dir"]:
                assert any(str(caller) in line and operation in line for line in calls), (operation, calls)
            assert before == after, ("later refusal changed protected state", before, after, rejected.stdout)
            for path, original in saved.items():
                if original is None:
                    path.unlink()
                else:
                    path.write_bytes(original)
            if not updates_existed:
                pending.parent.rmdir()
            print("PASS: later_role_refusal_preserves_recognized_clones_and_install", flush=True)

        if args.case in ["all", "refusal"]:
            if windows:
                print("SKIP: later role-refusal witness exercises the Unix shared-install gate", flush=True)
            else:
                later_refusal()
        result, _ = install()
        tree = g(caller, "mktree").stdout.strip()
        clean = g(caller, "commit-tree", tree, "-m", "fixture").stdout.strip()
        for clone, clean_name in ([(caller, "clean-caller"), (checkout, "clean-installed")] if args.case in ["all", "enrollment"] else []):
            # Tiny synthetic snapshots bound the proof; the Git-launched hook still belongs to the real checkout.
            tree = g(clone, "mktree").stdout.strip()
            clean = g(clone, "commit-tree", tree, "-m", "fixture").stdout.strip()
            # Positive execution control precedes refusal; no missing-interpreter failure counts as a scan.
            g(clone, "push", remote, f"{clean}:refs/heads/{clean_name}")
            before_refs = g(remote, "for-each-ref", "--format=%(refname) %(objectname)").stdout
            refused = g(clone, "push", remote, f"{clean}:refs/heads/{denied_name}", ok=False)
            after_refs = g(remote, "for-each-ref", "--format=%(refname) %(objectname)").stdout
            expected = f"pre-push: branch update 1 (object {clean}) holds the list entry at line 2; refusing the push"
            assert refused_with(refused, expected) and before_refs == after_refs, (
                f"ref refusal: exit {refused.returncode}\n{refused.stdout}\nrefs before: {before_refs}\nrefs after: {after_refs}")
            blob = g(clone, "hash-object", "-w", "--stdin", data=denied_name + "\n").stdout.strip()
            tree = g(clone, "mktree", data=f"100644 blob {blob}\tpayload\n").stdout.strip()
            payload = g(clone, "commit-tree", tree, "-m", "fixture payload").stdout.strip()
            refused = g(clone, "push", remote, f"{payload}:refs/heads/payload", ok=False)
            after_refs = g(remote, "for-each-ref", "--format=%(refname) %(objectname)").stdout
            expected = f"pre-push: blob {blob} holds the list entry at line 2; refusing the push"
            assert refused_with(refused, expected) and before_refs == after_refs, (
                f"content refusal: exit {refused.returncode}\n{refused.stdout}\nrefs before: {before_refs}\nrefs after: {after_refs}")
            assert g(clone, "config", "--local", "--get", "sot.publishList").stdout.strip().replace("\\", "/") == str(policy).replace("\\", "/")
        if args.case in ["all", "enrollment"]:
            assert "publish guard: ready for" in result.stdout, result.stdout
        # Refused options never enroll even a recognized fresh caller clone.
        if args.case in ["all", "refusal"]:
            untouched = root / "untouched"
            g(root, "clone", origin, untouched)
            g(untouched, "checkout", "--detach", commit)
            before = (untouched / ".git/config").read_bytes()
            rejected = run(command + ["--unknown-option"], untouched, env, ok=False)
            assert rejected.returncode == 2, rejected.stdout
            assert (untouched / ".git/config").read_bytes() == before
            assert not (untouched / ".git/hooks/pre-push").exists()
        if args.case in ["all", "migration"]:
            g(caller, "config", "core.hooksPath", "foreign-hooks")
            before = (caller / ".git/config").read_bytes()
            result, verdict = install()
            assert verdict.startswith("ONE STEP LEFT:") and "preserved publish hook conflict" in verdict, result.stdout
            assert "config --local --unset-all core.hooksPath" in result.stdout, result.stdout
            assert (caller / ".git/config").read_bytes() == before
            g(caller, "config", "--unset-all", "core.hooksPath")
        # Removing only this fixture's synthetic list forces provisioning, never a replacement policy.
        if args.case in ["all", "policy"]:
            policy.unlink()
            result, verdict = install()
            assert not policy.exists()
            before_refs = g(remote, "for-each-ref", "--format=%(refname) %(objectname)").stdout
            refused = g(caller, "push", remote, f"{clean}:refs/heads/missing-policy", ok=False)
            after_refs = g(remote, "for-each-ref", "--format=%(refname) %(objectname)").stdout
            expected = "pre-push: sot.publishList must name a readable regular file without a symlink; refusing the push"
            assert (verdict.startswith("ONE STEP LEFT:") and "provision" in verdict
                    and refused_with(refused, expected) and before_refs == after_refs), (
                f"{result.stdout}\npolicy refusal: exit {refused.returncode}\n{refused.stdout}"
                f"\nrefs before: {before_refs}\nrefs after: {after_refs}")
        print(f"PASS: installer publish guard {args.scenario}, {args.case}: real orchestration and observed state")


if __name__ == "__main__":
    main()
