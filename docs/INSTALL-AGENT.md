# Ship of Tools — agent install runbook

You are a coding agent (Claude Code, Codex, or similar) with shell access,
asked to install **Ship of Tools** — an agentic development system for Julia
(https://github.com/kalidke/ship-of-tools). This document is written for
*you*: follow it top to bottom, adapt where your judgment says so, and keep
the human informed at each step. For the release-artifact path, the
deterministic engine underneath is `scripts/install.sh`; your job is the
judgment around it — choosing the right topology, verifying prerequisites, and
proving the result works.

## Which install is this?

Published GitHub Releases with prebuilt artifacts exist (v0.3.1+, Linux
x86_64 / Windows x86_64 / macOS aarch64), so the default install path is the
**release installer** below. Verify assets for the latest tag exist
(`gh release view --repo kalidke/ship-of-tools` or the Releases page) before
relying on them.

The **source path** is for contributors or when release assets don't match the
target platform: clone the repo to a working directory of the human's choice
and follow https://kalidke.github.io/ship-of-tools/dev/start/setup/ (rustup +
juliaup, `cargo build --release --manifest-path rust/Cargo.toml`, Julia env
instantiation; source builds stamp `-dev` and never self-update). Ask if their
intent is ambiguous.

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
uname -sm                 # Linux x86_64 or macOS arm64 for release artifacts
case "$(uname -s)" in
  Linux)  ldd --version | head -1 ;;   # frontend needs glibc >= 2.35
  Darwin) sw_vers -productVersion ;;   # macOS aarch64 artifact only
esac
command -v git curl tar   # all required
command -v node npm       # OPTIONAL — math rendering in markdown previews
command -v pdftoppm pdfinfo  # OPTIONAL (poppler-utils) — PDF previews
command -v ffmpeg         # OPTIONAL — video poster frames in previews
```

- **node/npm absent** → not a blocker: the installer skips the MathJax
  sidecar deps with a warning and math in markdown previews shows raw LaTeX.
  Tell the human; if they want math, install node and re-run (or run
  `npm ci` in `<checkout>/rust/backend/sidecars/mathjax`).
- **poppler-utils or ffmpeg absent** → not a blocker: the installer does not
  check for them. A PDF or video preview then shows a note naming the missing
  tool. Tell the human; install them with the system package manager on the
  backend machine if they want those previews.
- An upgrade to a tmux-free tag removes the old sot-tmux.service unit by
  itself.
- **Linux x86_64, glibc ≥ 2.35** → full install works.
- **Linux, older glibc** → only `--be-only` (the backend is static musl);
  the frontend must run on another machine.
- **Windows** → no bash installer. Use **§2b Windows frontend → remote
  backend** (release zip + repo scripts — no Rust toolchain needed), or build
  from source.
- **macOS (Apple Silicon)** → EXPERIMENTAL; say so. The tested use is
  `--backend <ssh-alias>`: frontend on the Mac, backend on a Linux box — ONE
  command, the installer writes a tunnel-opening launcher. The other roles
  install (no systemd on macOS: the local-role launcher starts `sotd` on
  demand), but agent sessions on a Mac backend are not supported yet, so
  recommend `--backend`. Intel Macs: from-source only.

## 2. Choose the topology

### 2.0 First: is there already an install here?

**Look before you ask.** A machine with a working install has already answered
this question, and re-answering it differently reconfigures a live system —
that is a real incident, not a hypothetical: a documented install run on a box
with a live backend re-roled it, and the first anyone noticed was a monitoring
pane quietly showing one host.

```bash
cat ~/.local/share/sot/install.json 2>/dev/null        # prior install here (no role: derived fresh, below)
sotd topology status 2>/dev/null                        # does the declared list already name this box?
systemctl --user is-active --quiet sotd.service && systemctl --user cat sotd.service 2>/dev/null | grep ExecStart
cat ~/.local/bin/sot-launch 2>/dev/null                 # does an existing launcher belong to another prefix?
```

- **A schema-1 manifest** → this is an UPGRADE. Same command (bump `--version`
  or omit it for latest); role is derived fresh each run (the declared list,
  else the role flag/interactive answer) — the manifest's own `daemon`/
  `frontend` bits just record what got installed, for a listless box's own
  self-update check to fall back on, same list-first order.
- **The backend service or any FE integration file belongs to a different
  prefix** → one ownership rule covers all of them: an ACTIVE `sotd.service`
  whose `ExecStart` is outside this prefix, the `~/.local/bin/sot-launch`
  wrapper, the desktop entry, and the macOS app — any one of these already
  pointing at a source checkout or another `--prefix` means installing here
  would replace or disable it. Stop and tell the human — unless they asked
  you to replace a source build with a release install, which is that
  authorization already; then proceed with the flag. A unit file alone
  (e.g. seen over a shared home, not active here) is not this case, and
  neither is a wrapper/desktop entry/app that already points at THIS prefix
  (that's an upgrade). A `sot-launch` wrapper whose shape the installer
  doesn't recognize is refused the same way, but **not** overridable with
  `--force-role-change` — its owner can't be identified at all, so tell the
  human to move it aside instead.
- **A manifest you cannot read** — truncated, unknown schema, recording a
  different prefix — is NOT the same as no install. Treat it as unknown state
  and stop; the installer does the same.
- **Neither** → a fresh install; carry on below.

Only when a live daemon or integration file from a different prefix is
blocking you, and the human has explicitly authorized stepping on it, add
`--force-role-change` to
the install command. Never add it to get past an error you did not understand.

### 2.1 Ask the human (one question, fresh installs)

> "Where should Ship of Tools run? (a) everything on this machine,
> (b) the UI here but the backend on a server you SSH to,
> (c) backend only on this machine (headless server)."

| Answer | Installer flags |
|--------|-----------------|
| (a) all-in-one | `--local` |
| (b) UI here, backend remote | `--backend <ssh-alias>` |
| (c) headless backend | `--be-only` (add `--no-service` for an optional shared-home deployment) |

For (b): verify key-based SSH first — `ssh -o BatchMode=yes <alias> true`.
If it fails, walk the human through `ssh-keygen` + `ssh-copy-id`, then
recheck. The *remote* machine also needs a `--be-only` install (offer to do
it over SSH after this one). **On a shared-home deployment**, always add
`--no-service`: a `systemd --user` unit written into a shared `$HOME` applies to
every host sharing it.

### 2.2 Quote what it touches, get explicit yes

Before running the installer (§3, or §2b's `install-shortcut.ps1` on Windows),
quote these facts to the human and wait for an explicit yes:

- Everything goes under their home directory, mostly `~/.local/share/sot`,
  plus user lingering (`loginctl enable-linger`) so the user-level `sotd`
  systemd service keeps the backend running after logout on Linux.
- Skills and hooks go into their global `~/.claude` and `~/.codex`. Hooks are
  merged into `~/.claude/settings.json` without removing existing ones. A
  skill of theirs with the same name as a shipped one (`julia-repl`,
  `show-result`, `sitrep`, `worktree`, `project-log`) is overwritten without a
  backup.
- Sessions start Claude Code in auto mode; Codex with approvals, the sandbox
  and hook trust all bypassed (`--dangerously-bypass-approvals-and-sandbox
  --dangerously-bypass-hook-trust`).
- There is no uninstall script; removal is manual.
- There is no isolated mode yet: install under a separate user account or in
  a VM if that matters to them.

## 2b. Windows frontend → remote backend

Windows is a first-class *frontend* host (the backend stays on Linux). No
packaged installer here yet (a packaged `install.ps1` is roadmap; no tracking
issue exists). Steps 1–4 are the minimal manual bring-up — good for proving
the connection once. **Do not stop there**: the end state for a Windows
machine is step 5 (the repo launcher + shortcut), which owns the tunnel,
keeps the frontend fresh, and puts the proper icon on the taskbar.

1. **Download + verify** from the selected release
   (https://github.com/kalidke/ship-of-tools/releases):
   `sot-<ver>-windows-x86_64.zip` + `SHA256SUMS`; check the hash
   (`Get-FileHash -Algorithm SHA256`), extract **all three** binaries —
   `sot.exe`, `sotd.exe`, `sot-capsule.exe` — into `%LOCALAPPDATA%\sot\bin`.
   Not just `sot.exe`: the launcher reads the install's version from the
   staged `sotd.exe` and treats its absence as a dev box (no pinned
   `repo\current`, so the local daemon has no resources and REPL verbs fail
   with "repl project missing"); `sot-apply.ps1` swaps and rolls back all
   three as a set.
2. **Backend**: install it on the Linux machine (`--be-only`, see the table
   above — you can drive that over SSH). It listens on that user's private
   socket, normally `/run/user/<uid>/sot/sessions/sot.sock`.
3. **Forward the protocol port** — local-only, terminating at the remote
   socket. This ONE forward is the whole tunnel: browser pages (Pluto, docs,
   video, WGLMakie) ride it through the daemon proxy (ADR 0035, v0.5.0+):

   ```powershell
   $sock = ssh <ssh-alias> '~/.local/share/sot/bin/sotd session-socket-path sot'
   ssh -N -L "18743:$sock" <ssh-alias>      # any free local port; 18743 is only an example
   ```

   Do NOT add the old fixed helper-port forwards (1234-1241) — they are
   retired, and on a shared host they can silently serve another user's
   content. `SOT_LEGACY_FORWARDS=1` in the launcher (step 5) is the escape
   hatch for a pre-v0.5.0 backend only.

4. **Launch**: `sot.exe --tcp 127.0.0.1:18743` (`sot.exe --help` prints the
   full flag set). Optionally persist the connection in
   `%LOCALAPPDATA%\sot\config\hosts.toml` (config discovery, one order, no
   repo-local layer: `$SOT_HOSTS` when set, else `%LOCALAPPDATA%\sot\config\hosts.toml`
   — `~/.config/sot/hosts.toml` on Linux/macOS).

   `sot.exe` does NOT open the SSH forward itself — the tunnel is yours (or
   a launcher's).

5. **Launcher + shortcut (the actual end state).** Never hand-roll a shortcut
   to `sot.exe --tcp ...` — that is a "naive" FE: nothing owns the tunnel or
   refreshes the remote `sotd`, there is no ADR-0017 exit-75 self-relaunch,
   and the taskbar shows a generic icon. The canonical Windows launcher is
   `scripts/launch-sot.ps1` in the repo, and the shortcut that wires it up is
   created by `scripts\install-shortcut.ps1`. They need a **clone for the
   scripts and config — NOT a Rust build**: when `rust\target\release` has no
   built frontend, the launcher runs the already-staged copy in
   `%LOCALAPPDATA%\sot\bin\sot.exe` — exactly where step 1 put the release
   zip's exe. (A source build is still supported and takes precedence when
   present.)

   ```powershell
   git clone https://github.com/kalidke/ship-of-tools
   cd ship-of-tools
   # hosts.toml is never hand-written here: it lives at
   # %LOCALAPPDATA%\sot\config\hosts.toml (or $SOT_HOSTS). -Hub names the
   # hub's ssh alias ONCE: install-shortcut.ps1 records it in install.json
   # and runs the box's FIRST `sotd topology sync --hub`, and the launcher
   # (scripts\launch-sot.ps1) refreshes the file from the hub on every
   # launch after that. Without -Hub a fresh box has no copy to learn the
   # hub from and launches local-only ("no hosts.toml ... yet: pass --hub").
   # The hub = "..." / [host.<name>] / daemon / frontend grammar is written
   # ONCE, on the hub, per docs/src/start/setup.md ("hosts.toml — the
   # declared topology"), or via the /sot-setup skill in a Claude Code session.
   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\install-shortcut.ps1 -Hub <hub-ssh-alias>
   ```

   Migrating off a source checkout: the box may look configured because
   `<repo>\.sot\hosts.toml` survives, but that repo-local layer is no longer
   read (step 4). `-Hub` is still required.

   `install-shortcut.ps1` also writes `%LOCALAPPDATA%\sot\install.json` (via
   `install-manifest.ps1`). **Do not skip it and hand-make a shortcut**: that
   file is what tells the frontend it is a release install, and without it the
   startup update check returns at its first guard and the machine can never
   update itself. It no-ops with an explanation on a `-dev` source build.

   `install-shortcut.ps1` creates `Desktop\Ship of Tools.lnk` →
   `%LOCALAPPDATA%\sot\repo\current\scripts\launch-sot.ps1` once that pinned
   checkout exists, else the clone's own `scripts\launch-sot.ps1` as a
   bootstrap fallback (`Get-SotLauncherTarget`; docs/adr/
   0030-versioning-release-and-auto-update.md's 2026-09-17 amendment — a
   version is binaries + resources + scripts, and the shortcut now tracks
   all three together). The launcher itself (opens the step-3 control
   forward, spawns/refreshes the remote `sotd`, applies any staged update
   via `sot-apply.ps1`, runs the frontend under the exit-75 respawn
   supervisor) sets the SoT icon (`logo.ico`, copied to `%LOCALAPPDATA%\sot`
   so it survives moving the clone), and stamps the AppUserModelID
   `ShipOfTools.Sot` on the `.lnk` so the running window merges into the
   shortcut's taskbar button with the right icon. Re-run it after pinning to
   the taskbar — it re-syncs the pin so it never drifts back to a naive
   `sot.exe`; the first launch after a fresh install migrates the pin onto
   the pinned checkout itself, with no by-hand step needed.

   The **first launch** finishes the layout the steps above leave incomplete:
   the launcher creates `%LOCALAPPDATA%\sot\repo\versions\v<ver>` (a detached
   worktree of this clone, pinned at the installed binaries' release tag) and
   the `repo\current` junction to it, because that junction is how the local
   daemon resolves its Julia resources (`julia/repl`, `julia/kernel`, the
   sidecars) and where the auto-updater prepares the next version — without
   it a release daemon falls through to the build machine's paths and REPL
   verbs fail with "repl project missing". A box installed before this
   existed gets the same on its next launch; every step is logged to
   `%LOCALAPPDATA%\sot\logs\supervisor.log` and a failure never stops the
   launch. That first launch also instantiates the checkout's Julia
   environments (`julia/kernel`, `julia/repl`, `julia/pluto`), which takes a
   few minutes — so a box that wants **local** REPLs or Pluto needs Julia on
   `PATH` (juliaup); without it the launcher says so in a startup notice and
   the frontend still runs against the remote backend.

## 2c. macOS

macOS artifacts ship with every release; use the installer — it handles macOS
natively (artifact selection, checksum via `shasum`, Gatekeeper de-quarantine,
launcher):

```bash
curl -fsSL https://raw.githubusercontent.com/kalidke/ship-of-tools/main/scripts/install.sh \
  | bash -s -- --backend <ssh-alias>     # FE on this Mac -> BE on a Linux box
  # or: --local (everything on the Mac) / --be-only (headless Mac backend)
```

`--backend` writes a `sot-launch` that opens the SSH control forward (local
port to the remote socket — the port is topology-derived (per OS user), NOT
18743: read it from the plan's `tunnel <host> <port>` line, as in §4;
browser pages ride it via the daemon proxy, ADR 0035; the legacy 1234-1241
forwards are opt-in via `SOT_LEGACY_FORWARDS=1` for pre-v0.5.0 backends) and
starts the frontend; `--local` and `--be-only` are not supported for agent
sessions on a Mac backend yet — use `--backend`. If the human prefers manual
steps: download `sot-<ver>-macos-aarch64.tar.gz` + `SHA256SUMS`, verify
(`shasum -a 256 -c`), `xattr -d com.apple.quarantine ./sot ./sotd`, forward
the ports as in 2b, `./sot --tcp 127.0.0.1:<port>` (no systemd on macOS;
launchd wiring is roadmap).

## 3. Install

> **Windows: skip this section.** `install.sh` covers Linux and macOS only —
> it exits with an error on MINGW/MSYS/Cygwin. Your install finished at the end
> of §2b; go straight to §4 and use the **Windows** branch there.

Before running the command below, confirm the human already said yes to
§2.2's "what it touches" facts — if they have not, quote them now and get
that yes first.

```bash
curl -fsSL https://raw.githubusercontent.com/kalidke/ship-of-tools/main/scripts/install.sh \
  | bash -s -- <FLAGS-FROM-STEP-2>
```

You are RIGHT to be wary of piping remote scripts to a shell — inspecting
first is encouraged: download it to a file, read it (it is ~500 commented
lines, everything under `$HOME`, no sudo, checksums verified before use),
then run the file with the same flags.

Two flags beyond the role: `--version vX.Y.Z` pins a specific release (the
leading `v` is required — the flag is used verbatim as the tag), and
`--prefix <dir>` relocates the install from `~/.local/share/sot` (substitute
your prefix in every path below if you use it).

Idempotent; re-running is also the upgrade path.
(If you fetched this runbook at a pinned commit, still use `main`'s installer as
above — it only resolves the release tag, then runs that release's own
`scripts/install.sh` unmodified, so this runbook stays compatible with any
release regardless of what main's copy currently says.) It downloads the
release binaries, verifies SHA256 checksums, clones
the repo at the release tag into
`~/.local/share/sot/repo/current` (blobless — small), installs Julia via
juliaup if missing, installs the agent comm resources with
`ShipTools.update_comm()` (Claude/Codex skills and `~/.sot-comm/bin`), and, for
backend roles, instantiates the Julia environments (`julia/kernel`,
`julia/repl`, and `julia/pluto`; Pluto is also precompiled/loaded for
first-open latency). It writes a `settings.toml` stub; `hosts.toml` is read,
or fetched with `--hub`, never written by the installer
(never clobbering user edits on same-role re-runs), and wires a `sot-launch`
wrapper + desktop entry (frontend roles) or a systemd user unit (backend
roles, unless `--no-service`).

## 4. Verify (do not skip; report results)

### Windows (§2b installs)

Every command below is PowerShell. Run them from the clone. All five must
pass before you report success — the last two are the difference between a
working install and one that can never update itself.

```powershell
# 1. The frontend runs and reports a release version (NOT "-dev+<sha>",
#    which means a source build that will never self-update).
& "$env:LOCALAPPDATA\sot\bin\sot.exe" --version

# 2. The shortcut exists and carries the Ship of Tools icon, not PowerShell's.
$sc = (New-Object -ComObject WScript.Shell).CreateShortcut("$env:USERPROFILE\Desktop\Ship of Tools.lnk")
$sc.TargetPath; $sc.Arguments; $sc.IconLocation   # IconLocation must end in logo.ico,0
Test-Path ($sc.IconLocation -replace ',0$')       # must be True

# 3. The install manifest exists — WITHOUT it the frontend never checks for
#    updates (it bails at its "not a release install" guard).
Get-Content "$env:LOCALAPPDATA\sot\install.json"  # role must be "remote"

# 4. The update applier is present and loads under Windows PowerShell 5.1.
$e = $null
[void][System.Management.Automation.Language.Parser]::ParseFile(
    (Resolve-Path .\scripts\sot-apply.ps1), [ref]$null, [ref]$e)
$e.Count   # must be 0

# 5. The tunnel is up. The port is topology-derived (per OS user), NOT 18743:
#    read it from the plan's "tunnel <host> <port>" line.
$port = (& "$env:LOCALAPPDATA\sot\bin\sotd.exe" topology plan |
         Select-String '^tunnel ' | Select-Object -First 1 | ForEach-Object { ($_ -split '\s+')[2] })
Test-NetConnection 127.0.0.1 -Port $port | Select-Object TcpTestSucceeded
```

Then launch from the desktop shortcut (not `sot.exe` directly — that is a
"naive" frontend with no tunnel and no self-relaunch). A native window should
open and connect. Confirm the **bottom border line** shows
`fe <version> · be <version>`: dark gray means the two halves agree, **yellow
means the frontend and backend are on different builds** and the backend
needs updating. Tell the human: **press `?` for help; the top line of the nav
pane always shows the pane-switch keys.**

If step 3 printed nothing, run `powershell -File scripts\install-manifest.ps1
-Hub <hub-ssh-alias>` and re-check. If it reports a `-dev` build, that is a source install: it
updates via the launcher's git pull + cargo rebuild instead, which is correct
and needs no manifest.

### Linux / macOS

```bash
~/.local/share/sot/bin/sotd --version        # must print the release version
git -C ~/.local/share/sot/repo/current describe --tags   # must equal the tag
cat ~/.local/share/sot/install.json          # schema 1; role matches step 2
```

For backend roles, prove the daemon answers a hello. Two branches:

- **Service install** (default): `systemctl --user status sotd` should show
  active — the daemon is already running.
- **`--no-service` install**: no unit exists; boot the daemon yourself
  (the installer's final output also prints a supervise hint):

  ```bash
  ~/.local/share/sot/bin/sotd --project-root ~ --label sot &
  ```

  (`--project-root ~`, matching the systemd unit and the installer's own
  supervise hint — NOT the checkout, which the installer treats as read-only
  and refuses to update when dirty.)

Then probe its socket (success = it prints `backend answers: <the release
version>`). The probe needs `nc` — minimal server images may lack it; if
`command -v nc` fails, install it (or skip the probe and rely on the
systemd-active check), and do NOT report a healthy backend as dead on a
missing-`nc` box:

```bash
sock="$(~/.local/share/sot/bin/sotd session-socket-path sot)"
# Ask the binary what wire protocol it speaks -- don't hard-code the
# number here (the `protocol <N>` at the end of `sotd --version`'s
# parenthetical exists exactly so out-of-tree probes like this one never
# have to; see version_line's doc comment in rust/protocol/src/lib.rs).
# Omit the field when the version line prints none; the backend defaults it.
proto="$(~/.local/share/sot/bin/sotd --version | grep -oE 'protocol [0-9]+' | grep -oE '[0-9]+' || true)"
proto_field=""
[ -n "$proto" ] && proto_field="\"protocol\":$proto,"
tmp="$(mktemp "${TMPDIR:-/tmp}/sot-hello.XXXXXX")"
(
  printf '{"v":1,"id":1,"kind":"req","op":"hello","payload":{"client_id":"install-check","last_seen_revision":0,%s"app_version":"agent-install"}}\n' \
    "$proto_field" \
    | nc -U "$sock" > "$tmp"
) &
pid=$!
for _ in 1 2 3 4 5; do [ -s "$tmp" ] && break; sleep 1; done
kill "$pid" 2>/dev/null || true
wait "$pid" 2>/dev/null || true
ans="$(sed -n 's/.*"app_version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$tmp")"
rm -f "$tmp"
[ -n "$ans" ] || { echo "ERROR: no hello response from $sock"; exit 1; }
echo "backend answers: $ans"
```

For frontend roles: run `sot-launch` (it lives in `~/.local/bin` — if the
command isn't found, that directory isn't on the human's `PATH`; add it or
use the desktop entry / app bundle the installer also created). A native
window should open and connect. Tell the human: **press `?` for help; the
top line of the nav pane always shows the pane-switch keys.**

## 5. Troubleshooting (the known failure modes)

| Symptom | Cause → fix |
|---------|-------------|
| `glibc >= 2.35` error | distro too old for the prebuilt frontend → use `--be-only` here + frontend elsewhere, or build from source |
| checksum verification FAILED | truncated download → re-run; still failing = report, don't bypass |
| `ssh ... doesn't work` during (b) | no key auth → `ssh-copy-id` then re-run |
| dirty-checkout refusal on upgrade | the human edited `repo/current` → `git -C ... stash` (or commit), re-run |
| local port (from the plan's `tunnel` line) already bound | another tunnel owns it → `--port <n>` |
| `topology sync failed: no hosts.toml ... yet: pass --hub <alias>` | the box never had its first sync (fresh install without `-Hub`, or migrated off a checkout whose `.sot\hosts.toml` is no longer read) → `sotd topology sync --hub <alias>` (or re-run `install-shortcut.ps1 -Hub <alias>`), then relaunch |
| backend socket missing | old TCP-based service unit or failed daemon start → reinstall/restart the socket-based `sotd.service` |
| Julia instantiate fails "project and manifest are out of sync" (often naming a stdlib, e.g. `Sockets`) | stale `Manifest.toml` from a previous install left in the checkout's env dirs → re-run the installer (it drops stale manifests since 2026-08-11); manual fix: `rm ~/.local/share/sot/repo/current/julia/{kernel,repl,pluto}/Manifest.toml` and re-run |
| Julia instantiate slow on first run | normal (precompilation); minutes, once |
| session rows die / capsule pane blinks "supervisor lane not answering" forever, journal claims a start that produced no process | `$HOME` or the state dir is on a remote filesystem (NFS/CIFS/SMB2/9p/FUSE) — capsule records need a local disk (ADR 0043 decision 23) → set `XDG_STATE_HOME` to a local-disk directory (`systemctl --user set-environment XDG_STATE_HOME=/path`, or export it in `~/.bashrc`, which the unit sources) and restart `sotd` |

## 6. After the install

- The checkout **is the manual**: point yourself (and the human) at
  `~/.local/share/sot/repo/current/docs/USING.md` — on Windows that is
  `docs\USING.md` in the clone made in §2b step 5. Inside the app, the
  terminal-drawer agent gets the right path via `$SOT_MANUAL` on every
  platform.
- Updating later:
  - **Linux / macOS** — re-run step 3. The app also notifies about new
    releases and stages fresh binaries itself; `sot-launch` applies them at the
    next start via `sot-apply`.
  - **Windows** — step 3 does not run here. The frontend checks for releases
    at startup and stages them; `launch-sot.ps1` calls `scripts\sot-apply.ps1`
    on the next launch to verify and swap the binaries, keeping `.prev` and
    rolling back automatically if the new build crash-loops within 10s. This
    needs `install.json` to exist (§4 step 3) — without it the check never
    runs. Scripts and config update the SAME way: the shortcut/pin targets
    `%LOCALAPPDATA%\sot\repo\current\scripts\launch-sot.ps1` (docs/adr/
    0030-versioning-release-and-auto-update.md's 2026-09-17 amendment), and
    `sot-apply.ps1`'s junction flip carries them in the same transaction as
    the binaries — never a separate `git pull` of the clone.
  - Source builds are stamped `-dev` and never self-update on any platform;
    they update by pulling and rebuilding.
- Uninstall: `rm -rf ~/.local/share/sot ~/.config/sot ~/.local/bin/sot-launch`;
  if a unit was installed, `systemctl --user disable --now sotd` and remove
  `~/.config/systemd/user/sotd.service`; also remove the desktop entry
  (`~/.local/share/applications/ship-of-tools.desktop`, plus
  `~/.local/share/icons/hicolor/256x256/apps/ship-of-tools.png`) or the macOS
  app (`~/Applications/Ship of Tools.app`).
  On **Windows**: `Remove-Item -Recurse "$env:LOCALAPPDATA\sot"` (binaries,
  install.json, updates, logs), delete `"$env:APPDATA\sot"` if you wrote a
  config there, remove `"$env:USERPROFILE\Desktop\Ship of Tools.lnk"` and
  unpin the taskbar shortcut, then delete the clone. Agent comm resources written by
  `ShipTools.update_comm()` (`~/.sot-comm`, skills under `~/.claude` /
  `$CODEX_HOME` (default `~/.codex`), launchers in `~/.local/bin`) are shared with other checkouts —
  remove them only if this was the machine's only Ship of Tools install.
