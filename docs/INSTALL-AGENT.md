# Ship of Tools — agent install runbook

You are a coding agent (Claude Code, Codex, or similar) with shell access,
asked to install **Ship of Tools** — an agentic development system for Julia
(https://github.com/kalidke/ship-of-tools). This document is written for
*you*: follow it top to bottom, adapt where your judgment says so, and keep
the human informed at each step. The deterministic engine underneath is
`scripts/install.sh`; your job is the judgment around it — choosing the
right topology, verifying prerequisites, and proving the result works.

## Which install is this?

This runbook produces the **user install**: prebuilt binaries + a
release-tag-pinned checkout that serves as resource tree and manual. If the
human says they want to **develop Ship of Tools itself** (hack on the source,
build from main), do NOT use this runbook — clone the repo to a working
directory of their choice and follow
https://kalidke.github.io/ship-of-tools/dev/start/setup/ instead (rustup +
juliaup, `cargo build --release --manifest-path rust/Cargo.toml`, Julia env
instantiation; source builds stamp `-dev` and never self-update). Ask if
their intent is ambiguous.

## 0. Ground rules

- Don't run anything as root; everything installs under `$HOME`
  (`~/.local/share/sot`, `~/.config/sot`, `~/.local/bin`). The installer
  honors `SOT_PREFIX` and `XDG_CONFIG_HOME` overrides — if you set them,
  substitute your prefix in every path below (verify, uninstall, all of it).
- Use the installer's **flags**, not its interactive TTY Q&A — you are the
  interactive layer now.
- Report what you find and decide; ask the human only what you cannot infer
  (primarily: which topology they want, and SSH details for remote setups).
- **Lead with what WILL work for the human's machine and topology** — state
  the working path first, then caveats. A wall of warnings before any plan
  reads as "this doesn't work" to a human who just wants to install.

## 1. Preflight (report findings before proceeding)

```bash
uname -sm                 # need Linux x86_64 for prebuilt binaries
ldd --version | head -1   # frontend needs glibc >= 2.35 (backend is static)
command -v git curl tar   # all required; jq required if gh is absent
```

- **Linux x86_64, glibc ≥ 2.35** → full install works.
- **Linux, older glibc** → only `--be-only` (the backend is static musl);
  the frontend must run on another machine.
- **Windows** → the prebuilt frontend ships in every release; the backend
  runs on a Linux machine. Follow **§2b Windows frontend → remote backend**
  below (no bash installer involved). Building from source is the dev path
  (https://kalidke.github.io/ship-of-tools/dev/start/setup/).
- **macOS (Apple Silicon)** → fully supported by the installer, same three
  topologies as Linux (EXPERIMENTAL — artifacts build green but are lightly
  dogfooded; say so, then proceed). `--backend <ssh-alias>` is the common
  want: frontend on the Mac, backend on a Linux box — ONE command, the
  installer writes a tunnel-opening launcher. No systemd on macOS: the
  local-role launcher starts `sotd` on demand instead. Intel Macs:
  from-source only.

## 2. Choose the topology (ask the human, one question)

> "Where should Ship of Tools run? (a) everything on this machine,
> (b) the UI here but the backend on a server you SSH to,
> (c) backend only on this machine (headless server)."

| Answer | Installer flags |
|--------|-----------------|
| (a) all-in-one | `--local` |
| (b) UI here, backend remote | `--backend <ssh-alias>` |
| (c) headless backend | `--be-only` (add `--no-service` if this $HOME is NFS-shared across machines) |

For (b): verify key-based SSH first — `ssh -o BatchMode=yes <alias> true`.
If it fails, walk the human through `ssh-keygen` + `ssh-copy-id`, then
recheck. The *remote* machine also needs a `--be-only` install (offer to do
it over SSH after this one). **On a shared/NFS `$HOME`** (HPC clusters — if
`df -T ~ | grep -q nfs` hits), always add `--no-service`: a `systemd --user`
unit written into a shared `$HOME` applies to EVERY node sharing it.

## 2b. Windows frontend → remote backend

Windows is a first-class *frontend* host (the backend stays on Linux). No
bash installer here — four steps (issue #23):

1. **Download + verify** from the latest release
   (https://github.com/kalidke/ship-of-tools/releases):
   `sot-<ver>-windows-x86_64.zip` + `SHA256SUMS`; check the hash
   (`Get-FileHash -Algorithm SHA256`), extract `sot.exe` somewhere stable
   (e.g. `%LOCALAPPDATA%\sot\bin`).
2. **Backend**: install it on the Linux machine (`--be-only`, see the table
   above — you can drive that over SSH). It listens on `127.0.0.1:18743`
   only.
3. **Forward the ports** — not just the protocol port; the aux ports carry
   Pluto, video, and the docs-site servers (`W` and `o` silently break
   without them):

   ```powershell
   ssh -N -L 18743:127.0.0.1:18743 -L 1234:127.0.0.1:1234 -L 1235:127.0.0.1:1235 `
       -L 1236:127.0.0.1:1236 -L 1237:127.0.0.1:1237 -L 1238:127.0.0.1:1238 `
       -L 1239:127.0.0.1:1239 -L 1240:127.0.0.1:1240 <ssh-alias>
   ```

4. **Launch**: `sot.exe --tcp 127.0.0.1:18743` (`sot.exe --help` prints the
   full flag set since v0.3.1). Optionally persist the connection in
   `%APPDATA%\sot\hosts.toml` (config discovery: `$SOT_HOSTS` →
   `<repo>/.sot/hosts.toml` → `%APPDATA%\sot\hosts.toml`).

   `sot.exe` does NOT open the SSH forward itself — the tunnel is yours (or
   a launcher's). The dev repo's `scripts/launch-sot.ps1` automates
   forward + launch + respawn for source checkouts; a packaged `install.ps1`
   is tracked in issue #23.

## 2c. macOS

Use the installer — it handles macOS natively (artifact selection,
checksum via `shasum`, Gatekeeper de-quarantine, launcher):

```bash
curl -fsSL https://raw.githubusercontent.com/kalidke/ship-of-tools/main/scripts/install.sh \
  | bash -s -- --backend <ssh-alias>     # FE on this Mac -> BE on a Linux box
  # or: --local (everything on the Mac) / --be-only (headless Mac backend)
```

`--backend` writes a `sot-launch` that opens the SSH forwards (18743 +
1234-1240) and starts the frontend; `--local` starts `sotd` on demand (no
systemd on macOS; launchd wiring is roadmap). If the human prefers manual
steps: download `sot-<ver>-macos-aarch64.tar.gz` + `SHA256SUMS`, verify
(`shasum -a 256 -c`), `xattr -d com.apple.quarantine ./sot ./sotd`, forward
the ports as in 2b, `./sot --tcp 127.0.0.1:18743`.

## 3. Install

```bash
curl -fsSL https://raw.githubusercontent.com/kalidke/ship-of-tools/main/scripts/install.sh \
  | bash -s -- <FLAGS-FROM-STEP-2>
```

You are RIGHT to be wary of piping remote scripts to a shell — inspecting
first is encouraged: download it to a file, read it (it is ~300 commented
lines, everything under `$HOME`, no sudo, checksums verified before use),
then run the file with the same flags.

Idempotent; re-running is also the upgrade path. (If you fetched this
runbook at a pinned commit, still use `main`'s installer as above — the
installer is the moving part and stays compatible with this runbook.) It downloads the release
binaries, verifies SHA256 checksums, clones the repo at the release tag into
`~/.local/share/sot/repo/current` (blobless — small), installs Julia via
juliaup if a backend role needs it and Julia is missing, instantiates the
Julia environments, writes `~/.config/sot/hosts.toml` + `settings.toml`
(never clobbering user edits on same-role re-runs), and wires a `sot-launch`
wrapper + desktop entry (frontend roles) or a systemd user unit (backend
roles, unless `--no-service`).

## 4. Verify (do not skip; report results)

```bash
~/.local/share/sot/bin/sotd --version        # must print the release version
git -C ~/.local/share/sot/repo/current describe --tags   # must equal the tag
```

For backend roles, prove the daemon answers a hello. Two branches:

- **Service install** (default): `systemctl --user status sotd` should show
  active — the daemon is already running.
- **`--no-service` install**: no unit exists; boot the daemon yourself
  (the installer's final output also prints a supervise hint):

  ```bash
  ~/.local/share/sot/bin/sotd --tcp 127.0.0.1:<port> \
      --project-root ~/.local/share/sot/repo/current --label sot &
  ```

Then probe (**substitute your `--port` value** for 18743 if you set one;
success = it prints `backend answers: <the release version>`):

```bash
python3 - <<'EOF'
import json, socket
s = socket.create_connection(("127.0.0.1", 18743), timeout=5)
s.sendall((json.dumps({"v":1,"id":1,"kind":"req","op":"hello","payload":
  {"client_id":"install-check","last_seen_revision":0,"protocol":1,
   "app_version":"agent-install"}})+"\n").encode())
print("backend answers:", json.loads(s.makefile().readline())["payload"]["app_version"])
EOF
```

For frontend roles: run `sot-launch`; a native window should open and
connect. Tell the human: **press `?` for help; the top line of the nav pane
always shows the pane-switch keys.**

## 5. Troubleshooting (the known failure modes)

| Symptom | Cause → fix |
|---------|-------------|
| `glibc >= 2.35` error | distro too old for the prebuilt frontend → use `--be-only` here + frontend elsewhere, or build from source |
| checksum verification FAILED | truncated download → re-run; still failing = report, don't bypass |
| `ssh ... doesn't work` during (b) | no key auth → `ssh-copy-id` then re-run |
| GitHub API rate-limit (60/h per IP) | authenticate `gh` or set `$GITHUB_TOKEN` (optional otherwise) |
| dirty-checkout refusal on upgrade | the human edited `repo/current` → `git -C ... stash` (or commit), re-run |
| port 18743 already bound | another sotd or a tunnel owns it → `--port <n>` |
| Julia instantiate slow on first run | normal (precompilation); minutes, once |

## 6. After the install

- The checkout **is the manual**: point yourself (and the human) at
  `~/.local/share/sot/repo/current/docs/USING.md` — inside the app, the
  terminal-drawer agent gets the same path via `$SOT_MANUAL`.
- Updating later: re-run step 3 (the app also notifies about new releases
  and stages fresh binaries itself).
- Uninstall: `rm -rf ~/.local/share/sot ~/.config/sot ~/.local/bin/sot-launch`
  and `systemctl --user disable --now sotd` if a unit was installed.
