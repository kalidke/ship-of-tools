#!/usr/bin/env python3
"""Real sotd install orchestration in scratch profiles, with actual Git-launched pushes.

Run after building the three binaries, on Linux/macOS/Windows. --scenario repair is also a baseline witness against
4464089f7's binaries and checkout: successful orchestration there leaves both known clones unenrolled. Julia steps
and the Windows shortcut are recording fixtures; no row, service, window, agent or network request is started.
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
import tempfile


def run(argv, cwd, env, ok=True, input_text=None):
    result = subprocess.run([str(a) for a in argv], cwd=cwd, env=env, text=True,
                            stdin=subprocess.DEVNULL if input_text is None else None, input=input_text,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=180)
    if ok and result.returncode:
        raise AssertionError(f"{argv}: exit {result.returncode}\n{result.stdout}")
    return result


def refused_with(result, expected_line):
    guard_lines = [line for line in result.stdout.splitlines() if line.startswith("pre-push: ")]
    return result.returncode == 1 and guard_lines == [expected_line]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--sotd", type=Path, required=True)
    parser.add_argument("--sot", type=Path, required=True)
    parser.add_argument("--capsule", type=Path, required=True)
    parser.add_argument("--scenario", choices=["fresh", "repair"], default="repair")
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
            jq = shutil.which("jq.exe")
            if not jq:
                raise RuntimeError("native jq.exe is a fixture prerequisite")
            shutil.copy2(jq, tools / "jq.exe")
            system = Path(env["SYSTEMROOT"]) / "System32"
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

        def g(cwd, *words, ok=True, data=None):
            return run([git, "-C", cwd, *words], cwd=root, env=env, ok=ok, input_text=data)

        version_line = run([binaries["sotd"], "--version"], root, env).stdout
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
            result = run(command, cwd, env, ok=success)
            lines = [line for line in result.stdout.splitlines() if re.match(r"^(PASS|FAIL|ONE STEP LEFT): ", line)]
            assert len(lines) == 1 and result.stdout.rstrip().endswith(lines[0]), result.stdout
            if success:
                assert not lines[0].startswith("FAIL:"), result.stdout
                if args.scenario == "repair":
                    assert "a repair run" in result.stdout, result.stdout
            return result, lines[0]

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
