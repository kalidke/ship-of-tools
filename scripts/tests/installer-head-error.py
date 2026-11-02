#!/usr/bin/env python3
"""Run native apply against unreadable, untrusted and mismatched prepared checkouts.

Linux/macOS need Git and sh; Windows needs Git and Windows PowerShell 5.1.
"""

import hashlib
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile

sys.dont_write_bytecode = True
from installer_process import capture


def witness(case):
    repo = Path(__file__).resolve().parents[2]
    windows = os.name == "nt"
    target = "windows-x86_64" if windows else ("macos-aarch64" if sys.platform == "darwin" else "linux-x86_64")
    with tempfile.TemporaryDirectory(prefix="sot-head-error-") as scratch:
        root = Path(scratch).resolve()
        home, prefix, checkout = (root / name for name in ["home", "prefix", "checkout"])
        for folder in [home, prefix / "bin", checkout]:
            folder.mkdir(parents=True)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("SOT_", "GIT_", "XDG_"))}
        env.update(HOME=str(home), USERPROFILE=str(home), LOCALAPPDATA=str(home / "local"),
                   APPDATA=str(home / "config"), SOT_APPLY_REEXECED="1", LC_ALL="C",
                   GIT_CONFIG_GLOBAL=str(root / "gitconfig"), GIT_CONFIG_SYSTEM=str(root / "gitconfig"),
                   GIT_CONFIG_NOSYSTEM="1", GIT_TERMINAL_PROMPT="0")
        (root / "gitconfig").write_text("", encoding="ascii")
        if case == "unreadable":
            (checkout / ".git").write_text("invalid git metadata\n", encoding="ascii")
        else:
            for words in [["init", "--quiet"], ["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                                              "commit", "--quiet", "--allow-empty", "-m", "fixture"]]:
                result = capture(["git", "-C", str(checkout), *words], root, env, timeout=15)
                assert result.returncode == 0, result.stdout
            if case == "untrusted":
                env["GIT_TEST_ASSUME_DIFFERENT_OWNER"] = "1"
        git = capture(["git", "-C", str(checkout), "rev-parse", "HEAD"], root, env, timeout=15)
        if case == "mismatched":
            assert git.returncode == 0, git.stdout
        else:
            assert git.returncode != 0 and "fatal:" in git.stdout, git.stdout
        reason = " ".join(git.stdout.split())
        tag = "v9.9.9"
        asset = f"sot-9.9.9-{target}" + (".zip" if windows else ".tar.gz")
        ready = prefix / "updates" / f"{tag}-{target}"
        staged = ready / asset.removesuffix(".zip").removesuffix(".tar.gz")
        staged.mkdir(parents=True)
        for name in ["sot", "sotd"]:
            (staged / (name + (".exe" if windows else ""))).write_bytes(b"staged binary\n")
        (ready / "manifest.json").write_text("{}\n", encoding="ascii")
        (ready / asset).write_bytes(b"archive\n")
        record = b'{\n  "tag": "v9.9.8",\n  "version": "9.9.8"\n}\n'
        (prefix / "install.json").write_bytes(record)
        pointer = prefix / "updates" / f"pending-{target}.json"
        pointer.write_text(json.dumps(dict(tag=tag, target=target, checkout=str(checkout), commit="a" * 40,
                                          asset=asset, asset_sha256=hashlib.sha256(b"archive\n").hexdigest()),
                                      indent=2) + "\n", encoding="ascii")
        if windows:
            # A hosted step's pwsh leaves PowerShell 7's module path, where 5.1's Get-FileHash fails to load.
            system = Path(os.environ["SystemRoot"]) / "System32"
            env["PSModulePath"] = str(system / "WindowsPowerShell/v1.0/Modules")
            command = ["powershell.exe", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File",
                       str(repo / "scripts/sot-apply.ps1"), "-Prefix", str(prefix)]
        else:
            apply = prefix / "bin/sot-apply"
            shutil.copy2(repo / "scripts/sot-apply.sh", apply)
            command = ["sh", str(apply)]
        result = capture(command, root, env, timeout=30)
        lines = [line for line in result.stdout.splitlines() if "prepared checkout HEAD" in line]
        assert result.returncode == 0 and len(lines) == 1 and reason in lines[0], result.stdout
        assert not pointer.exists(), result.stdout
        assert (prefix / "install.json").read_bytes() == record
        assert ready.is_dir() and not (prefix / "bin/sot.prev").exists()
        assert not (prefix / "updates/.lock").exists()
        print(f"PASS: {case} HEAD: apply keeps Git's output on one line and drops only the pointer")


if __name__ == "__main__":
    for case in ["unreadable", "untrusted", "mismatched"]:
        witness(case)
