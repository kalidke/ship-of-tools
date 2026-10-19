# Ship of Tools — agent install runbook

You are a coding agent (Claude Code, Codex, or similar) with shell access,
asked to install **Ship of Tools** — an agentic development system for Julia
(https://github.com/kalidke/ship-of-tools). This document is written for
*you*: follow it top to bottom, adapt where your judgment says so, and keep
the human informed at each step. For the release-artifact path, the
deterministic engine underneath is `scripts/install.sh`; your job is the
judgment around it — choosing the right topology, verifying prerequisites, and
proving the result works.

## Update this computer (Linux and macOS)

If Ship of Tools is already installed here (`~/.local/share/sot/install.json` exists) and the human says "update sot",
"update sot from the repo" or "update Ship of Tools", run the installer from the installed checkout, with no options:

```bash
bash ~/.local/share/sot/repo/current/scripts/install.sh
```

(`--prefix <dir>` or `SOT_PREFIX` if the install is not under `~/.local/share/sot`.) With no `--version` it follows the
channel this computer already runs: a stable release takes the latest stable release; a pre-release takes the newest
published release of any kind. The role is `hosts.toml`'s entry for this computer first (it wins over a role flag), then
the role flag, then what the install recorded, so an update needs no `--local`, `--backend` or `--be-only`. It never moves the install backwards unless told (`--version vX.Y.Z` names the target; an older one is then
said to be older and installed, and a run that names a version pins the install). A run can take many minutes (the
Julia environments are prepared), longer than a tool call's default timeout: run it in the background or with the
longest timeout the tool allows. It ends with exactly one line; read the last line of its output, and act on it:

- `PASS: Ship of Tools <v> at <short> on this computer`: done. Say so.
- `ONE STEP LEFT: <step>`: the install is whole and one thing remains that only the human can do (usually: quit the
  window and start it again, since it still runs the old version). Tell the human that step in those words.
- `FAIL: <check>: <detail>`: stop and report the line. Three are decisions, not faults. `daemon:` names rows with a live
  REPL and says when the Pluto server is running (a restart would end the REPLs and every notebook open in it; their
  variables are lost, the rows and their agents keep running), or says why the daemon could not be asked in full. A daemon that cannot answer is judged from its own REPL and Pluto child processes: with none, the run goes on with no flag. When something is live and a person is at the console, the installer asks "Restart the daemon? ... [y/N]" in place; with no console (an agent's run) it stops with this line: ask the human, and re-run with `--restart-daemon` only on a yes. `target:` and
  `direction:` are the human's to answer, never a version the agent picks. `target:` says the install is a build of the
  line (a `+` in its version): a pack of the line (`scripts/pack-build.sh` in the checkout that built it) goes in with
  `--archive <folder>`, while a release named with `--version` is usually older and moves this computer back.
  `direction:` says the newest release is older than what runs here: only a version the human names goes back. Ask the
  human which they want.

`--archive` runs the install.sh of the checkout that made the pack, not the installed one:
`bash <that checkout>/scripts/install.sh --archive <folder>`.

If a run stops half way (a lost connection, a Ctrl-C), run it again: `install.json` is written last, and a repeat is a
repair run that ends in `PASS`. With no network, `--archive <folder>` installs from a folder carried here (the base
clone still needs the repo's origin unless it already holds the commit).

An install old enough to predate this entry has an `install.sh` that does not end in one of those lines
(`grep -q 'ONE STEP LEFT' ~/.local/share/sot/repo/current/scripts/install.sh` says which). Read `version` in its
`install.json` first: a `+` in it is the `target:` case above, so ask the human; otherwise follow the channel (a stable
version takes the latest stable release, a pre-release the newest published one), tell the human the version, and run
that tag's installer with no version: `curl -fsSL https://raw.githubusercontent.com/kalidke/ship-of-tools/<tag>/scripts/install.sh | bash`.
It follows the same channel itself, so the install stays unpinned and its automatic updates stay on (`--version` would pin it).
A source checkout (no `install.json`) updates by `git pull` and a rebuild. With no install here at all, go on to the next
sections.

## Update this computer (Windows)

If `%LOCALAPPDATA%\sot\install.json` exists and the human says "update sot", "update sot from the repo" or "update Ship
of Tools", run the installer from the installed checkout, with no options (`-Prefix <dir>` if the install is elsewhere).
The versions path works from an ssh session too, where `repo\current` cannot be traversed:

```powershell
$p="$env:LOCALAPPDATA\sot"; $t=(Get-Content "$p\install.json" -Raw | ConvertFrom-Json).tag
powershell -NoProfile -ExecutionPolicy Bypass -File "$p\repo\versions\$t\scripts\install.ps1"
```

It follows the channel this computer already runs, exactly as the Unix installer does (a stable release takes the latest
stable release; a pre-release the newest published one; a `+` build stops), and never moves the install backwards unless
told (`-Version vX.Y.Z` names the target; an older one is then said to be older and installed, and a run that names a
version pins the install). A run can take many minutes (the stage prepares Julia, and a converge waits up to 30 minutes
for the window), longer than a tool call's default timeout: run it in the background or with the longest timeout the tool
allows. It ends with exactly one line; read the last line of its output, and act on it:

- `PASS: Ship of Tools <v> at <short> on this computer`: done. Say so.
- `ONE STEP LEFT: <step>`: the install is whole or the update is armed, and one thing remains that only the human can do:
  quit Ship of Tools (choose Keep) and start it from the shortcut, start it at the desk, or install Julia or jq with the
  winget command the line names. Tell the human that step in those words, and run the installer again to confirm.
- `FAIL: <check>: <detail>`: stop and report the line. `daemon:` names rows with a live REPL and says when the Pluto
  server is running (a restart would end both; the rows and their agents keep running). A daemon that cannot answer is judged
  from its own REPL and Pluto child processes, and the installer asks "Restart the daemon?" in place at a console; with no
  console it stops with this line: ask the human, and run it again with `-RestartDaemon` only on a yes. `target:` and `direction:` are the human's to answer, never a version the agent
  picks: `target:` says the install is a build of the line (a `+` in its version), which a pack of the line
  (`scripts\pack-build.ps1` in the checkout that built it) updates with `-Archive <folder>`, while a release named with
  `-Version` is usually older and moves this computer back; `direction:` says the newest release is older than what runs
  here, and only a version the human names goes back. Ask the human which they want.

`-Archive` runs the install.ps1 of the checkout that made the pack, not the installed one:
`powershell -NoProfile -ExecutionPolicy Bypass -File <that checkout>\scripts\install.ps1 -Archive <folder>`. A run that
names what to install (`-Version`, `-Archive`) pins the install: automatic updates stay off until a run that names no
version, and the PASS or ONE STEP LEFT line ends "pinned at <v>" (tell the human when it does). If a run stops half way (a lost connection, a Ctrl-C), run it again: a repeat is a repair run that ends in `PASS`.

An install that predates `install.ps1` has no such file in its checkout. Read `version` in its `install.json` first: a `+`
in it is the `target:` case above, so ask the human; otherwise tell the human the version (a stable one takes the latest
stable release, a pre-release the newest published one) and fetch that tag's installer:
`curl.exe -fsSL -o "$env:TEMP\sot-install.ps1" https://raw.githubusercontent.com/kalidke/ship-of-tools/<tag>/scripts/install.ps1`,
then run it with no version, so the install stays unpinned: its prelude follows the same channel.

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
instantiation; a source build has a `+` in its version and never self-updates). Ask if their
intent is ambiguous.

## 0. Ground rules

- The installer needs no root: everything it writes is under `$HOME` (`~/.local/share/sot`, `~/.config/sot`,
  `~/.local/bin`, plus the agent folders in 2.2). Installing a missing prerequisite (`sudo apt-get install ...`) may
  need root. The installer
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
command -v git curl tar jq perl   # all required
command -v flock          # required on Linux (the comm inbox lock)
command -v node npm       # OPTIONAL — math rendering in markdown previews
command -v pdftoppm pdfinfo  # OPTIONAL (poppler-utils) — PDF previews
command -v ffmpeg         # OPTIONAL — video poster frames in previews
command -v Xvfb           # OPTIONAL, Linux backend — GLMakie in the REPL and Pluto
```

- **jq absent** → a blocker: the comm scripts read and write JSON with it,
  and the installer stops without it. Install it with
  `sudo apt-get install jq` (Linux), `brew install jq` (macOS) or
  `winget install jqlang.jq` (Windows).
- **perl or flock absent** → the installer does not check for them, but the comm scripts need `perl` (registry
  writes; the clock before bash 5, which is every Mac) and, on Linux, `flock`, and a session without them never sees
  its mail. Install them (`sudo apt-get install perl util-linux`, `brew install perl`) before you report success.
- **node/npm absent** → not a blocker: the installer skips the MathJax
  sidecar deps with a warning and math in markdown previews shows raw LaTeX.
  Tell the human; if they want math, install node and re-run (or run
  `npm ci` in `<checkout>/rust/backend/sidecars/mathjax`).
- **poppler-utils or ffmpeg absent** → not a blocker: the installer does not
  check for them. A PDF or video preview then shows a note naming the missing
  tool. Tell the human; install them with the system package manager on the
  backend machine if they want those previews.
- **Xvfb absent on a Linux backend** → not a blocker: the installer does not
  check for it. The REPL's first run then says it has no display of its own;
  CairoMakie and WGLMakie still work, GLMakie does not. Tell the human;
  `sudo apt-get install xvfb` (Debian/Ubuntu) adds it, and a REPL started
  afterward gets its display.
- **Linux x86_64, glibc ≥ 2.35** → full install works.
- **Linux, older glibc** → only `--be-only` (the backend is static musl);
  the frontend must run on another machine.
- **Windows** → no bash installer. Use **§2b Windows frontend → remote
  backend** (release zip + repo scripts — no Rust toolchain needed), or build
  from source.
- **macOS (Apple Silicon)** → EXPERIMENTAL; say so. Use `--backend <ssh-alias>`
  (frontend on the Mac, backend on a Linux box enrolled first; §2.1 (b)). The other roles install, but agent
  sessions on a Mac backend are not supported; see the Platforms section of the install page in the manual. Intel
  Macs: from-source only.

## 2. Choose the topology

### 2.0 First: is there already an install here?

**Look before you ask.** A machine with a working install has already answered
this question, and re-answering it differently reconfigures a live system.

```bash
cat ~/.local/share/sot/install.json 2>/dev/null        # prior install here (no role: derived fresh, below)
~/.local/share/sot/bin/sotd topology status 2>/dev/null   # (use your prefix) does the declared list already name this box?
systemctl --user is-active --quiet sotd.service && systemctl --user cat sotd.service 2>/dev/null | grep ExecStart
cat ~/.local/bin/sot-launch 2>/dev/null                 # does an existing launcher belong to another prefix?
```

- **A schema-1 manifest** → this is an UPGRADE: "Update this computer" at the top
  of this page. Role is derived fresh each run (the declared list first, which wins over a
  role flag; then the role flag; then what the manifest records: its `daemon`/`frontend` bits, its
  `service`, its `hub`, and a remote install's alias from its wrapper) — so an
  update needs no role flag; the installer also reads `daemon` and `service`
  back, so a run never records less than they say under a declared topology
  (below).
- **The backend service or any FE integration file belongs to a different
  prefix** → one ownership rule covers all of them: an ACTIVE `sotd.service`
  whose `ExecStart` is outside this prefix, the `~/.local/bin/sot-launch`
  wrapper, the desktop entry, and the macOS app — any one of these already
  pointing at a source checkout or another `--prefix` means installing here
  would replace or disable it (the running `sotd.service` is checked for every role; the wrapper, desktop entry and
  macOS app only when a frontend is being installed). Stop and tell the human — unless they asked
  you to replace a source build with a release install, which is that
  authorization already; then proceed with the flag. A unit file alone
  (e.g. seen over a shared home, not active here) is not this case, and
  neither is a wrapper/desktop entry/app that already points at THIS prefix
  (that's an upgrade). A `sot-launch` wrapper whose shape the installer
  doesn't recognize is refused the same way, as is a desktop entry or macOS app that does not launch
  `~/.local/bin/sot-launch`; neither is overridable with `--force-role-change`
  (an owner that can't be identified), so tell the human to move it aside instead.
- **`hosts.toml` runs no sotd on this host and an install is already at the
  prefix** (this host is not listed, or is listed with neither `daemon` nor
  `frontend`), or **the manifest records a daemon or its unit and this run
  would record none** (`--no-service` over a `systemd` record, or the role
  flags under a `hosts.toml` sotd cannot read) → the installer refuses and
  writes nothing under the prefix or `~/.local/bin`. The remedy depends on the
  refusal:
  - this host is not declared: declare it in `hosts.toml` (`frontend` for a
    machine that runs a window, `daemon` for one other machines dial) and
    re-run, or, on a home several hosts share, run the installer on a host it
    already declares. A machine with a home of its own is declared in the
    hub's `hosts.toml` and re-runs with `--hub <hub alias>`, which fetches it;
  - `--no-service` over a `systemd` record: drop `--no-service`;
  - `hosts.toml` cannot be read: fix it (`sotd topology status` names the line).

  Add `--force-role-change` only as an override, when the human says this
  machine's own role changed.
- **A manifest you cannot read** — truncated, unknown schema, recording a
  different prefix — is NOT the same as no install. Treat it as unknown state
  and stop. The installer does not check this for you: it reads only whether the manifest records a daemon and a
  systemd service, and overwrites it.
- **Neither** → a fresh install; carry on below.

Never add `--force-role-change` to get past an error you did not understand.

### 2.1 Ask the human (one question, fresh installs)

> "Where should Ship of Tools run? (a) everything on this machine,
> (b) the UI here but the backend on a server you SSH to,
> (c) backend only on this machine (headless server)."

| Answer | Installer flags |
|--------|-----------------|
| (a) all-in-one | `--local` |
| (b) UI here, backend remote | `--backend <ssh-alias>` |
| (c) headless backend | `--be-only` (add `--no-service` when something else supervises `sotd`) |

For (b): verify key-based SSH first — `ssh -o BatchMode=yes <alias> true`.
If it fails, walk the human through `ssh-keygen` + `ssh-copy-id`, then
recheck. The *remote* machine needs a `--be-only` install first (offer to do
it over SSH before this one).

(b) also needs the hub (the backend you name) to know this machine. The wrapper `--backend` writes passes no
`--dial` of its own: the launcher syncs `~/.config/sot/hosts.toml` from the hub and builds its dials from
`sotd topology plan`, and the sync refuses a hub whose `hosts.toml` does not list this machine. So, on the hub,
`~/.config/sot/hosts.toml` must list this machine as `[host.<this box>]` with `frontend = true` (see
`docs/ENROLLING-A-HOST.md`, or `/sot-setup` on the hub). Install and enroll the backend first, then run
`--backend`; `--be-only` writes no `hosts.toml`. A `--backend` install the hub does not list ends in
`FAIL: enroll:`, naming `sotd topology set add <this box> --frontend` to run on the hub with the hub's own `sotd`; run
it there, then run the installer again. **On a shared-home deployment** (one `$HOME` on
several hosts), `~/.config/sot/hosts.toml` must list every host that shares
it: the installer pins `sotd.service`, whose file and boot drop-in
(`default.target.d/sotd.conf`; the unit has no `[Install]`, so `enable` and `disable` change no file) every such
host sees, to the hosts that list declares `daemon` or `frontend`
(`sotd.service.d/topology.conf`), and the unit starts on no other. A host the
list leaves out, or lists with neither key, never starts it.

### 2.2 Quote what it touches, get explicit yes

Before running the installer (§3, or §2b's `install.ps1` on Windows),
quote these facts to the human and wait for an explicit yes:

- Everything goes under their home directory, mostly `~/.local/share/sot`,
  plus user lingering (`loginctl enable-linger`) so the user-level `sotd`
  systemd service keeps the backend running after logout on Linux.
- Skills go into `~/.claude/skills` and `$CODEX_HOME/skills`. Claude hooks are
  merged into `~/.claude/settings.json`, and into each
  `~/.claude-auth/<name>/settings.json` that exists, without removing existing
  ones. A skill of theirs with the same name as any of the 13 shipped ones
  (`julia-repl`, `project-log`, `reauth`, `show-result`, `sitrep`, `worktree`, and the `sot-*` skills) is
  overwritten without a backup.
- Codex side: `$CODEX_HOME/AGENTS.md` is written when absent or ours, and a local plugin with a marketplace file goes
  under `~/.agents/plugins/` (Codex hooks are that plugin).
- Launchers `ccb`, `ccx`, `sot-fe`, `show-result` and `sot-gh-auth` go into `~/.local/bin`.
- The installer declares the home folder as the folder-trust root (`sotd trust declare $HOME`): the daemon then
  marks Claude Code's trust dialog answered for every project folder under `$HOME` when it starts a session.
- With no Julia 1.12 or newer, it installs juliaup from the network if juliaup is absent (`curl
  https://install.julialang.org`; juliaup's defaults may edit shell startup files), else adds the 1.12 channel; it does
  not change juliaup's default channel. It also downloads Julia package depots.
- Sessions start Claude Code in auto mode; Codex with approvals, the sandbox
  and hook trust all bypassed (`--dangerously-bypass-approvals-and-sandbox
  --dangerously-bypass-hook-trust`).
- There is no uninstall script; removal is manual.
- There is no isolated mode yet: install under a separate user account or in
  a VM if that matters to them.

## 2b. Windows frontend → remote backend

Windows is a first-class *frontend* host (the backend stays on Linux). `scripts\install.ps1` is the whole install, and
the update (the first section above). A fresh box has no checkout yet, so fetch the installer of the latest stable release
(`<tag>`) and run it with no version; it takes the latest stable release itself:

```powershell
curl.exe -fsSL -o "$env:TEMP\sot-install.ps1" https://raw.githubusercontent.com/kalidke/ship-of-tools/<tag>/scripts/install.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File "$env:TEMP\sot-install.ps1" -Hub <hub-ssh-alias>
```

While the latest stable release has no `scripts/install.ps1` (the fetch fails, or the run ends `FAIL: target:`), the person
may choose a pre-release instead: fetch that tag's installer and run it with `-Version <rc tag>`, which pins the install,
so its automatic updates stay off until a run that names no version. Never name a version the person did not choose.

(It needs git and curl.exe on PATH; Julia and jq are for comm and local REPLs, and the last line names the winget command
when one is missing, on a fresh install and on an update alike.) What it does, so you can read its output:

1. **Download and verify.** `sot-<ver>-windows-x86_64.zip`, `SHA256SUMS` and `COMMIT` from the tag's release; the hashes
   are checked, and the three binaries (`sot.exe`, `sotd.exe`, `sot-capsule.exe`) go into `%LOCALAPPDATA%\sot\bin`
   together: the launcher reads the install's version from `sotd.exe`, and `sot-apply.ps1` swaps and rolls back all three
   as a set. `-Archive <folder>` installs a folder `scripts\pack-build.ps1` packed instead.
2. **The layout.** A blobless clone of the repo in `repo\base`, `repo\versions\v<ver>` as a detached worktree at the
   release's commit, and the `repo\current` junction to it: the junction is how the local daemon resolves its Julia
   resources (`julia/repl`, `julia/kernel`, the sidecars) and where the updater prepares the next version. It then runs the
   launcher's own layout code, which writes `install.json` and instantiates the Julia environments when Julia is on PATH,
   the comm install (`ShipTools.update_comm()`, which needs jq), and `scripts\install-shortcut.ps1 -Hub`, which creates
   `Desktop\Ship of Tools.lnk` -> `%LOCALAPPDATA%\sot\repo\current\scripts\launch-sot.ps1` and re-syncs an existing
   taskbar pin. **Never hand-make a shortcut to `sot.exe --dial ...`**: that is a "naive" frontend, nothing refreshes the
   remote `sotd`, and `install.json` is what tells the frontend it is a release install.
3. **Backend**: install it on the Linux machine (`--be-only`, see the table above; you can drive that over SSH). It
   listens on that user's private socket, normally `/run/user/<uid>/sot/sessions/sot.sock`. There is no port to forward:
   the daemon has no TCP listener, and reaching it means the frontend spawning its own `ssh` child and speaking the
   protocol over its stdio (C3, `docs/src/concepts/transport.md`); browser pages (Pluto, docs, video, WGLMakie) reach
   the browser through the window's page relay, one ssh login per browser connection (ADR 0035).
4. **hosts.toml** is never hand-written on this box: `-Hub` names the hub's ssh alias once, install.ps1 records it in
   `install.json` and runs the first `sotd topology sync --hub`, and the launcher refreshes the file from the hub on every
   launch (`%LOCALAPPDATA%\sot\config\hosts.toml`, or `$SOT_HOSTS`). The `hub = "..."` / `[host.<name>]` grammar is written
   once, on the hub, per `docs/src/start/setup.md` ("hosts.toml, the declared topology") or by the `/sot-setup` skill.
   Without `-Hub` the box launches local-only. To try the connection once by hand: `sot.exe --dial <name>=ssh:<ssh-alias>`.

The **first launch** from the shortcut starts the window under the exit-75 respawn supervisor and the box's own local
daemon; every layout step is logged to `%LOCALAPPDATA%\sot\logs\supervisor.log`. A box that wants **local** REPLs or
Pluto needs Julia on PATH (`winget install --id Julialang.Juliaup -e`); without it the frontend still runs against the
remote backend. The `+` of a source build never installs here: a fresh install is a release, and a pack of the line moves
an existing install with `-Archive`.

## 2c. macOS

macOS artifacts ship with every release; use the installer — it handles macOS
natively (artifact selection, checksum via `shasum`, Gatekeeper de-quarantine,
launcher):

```bash
curl -fsSL https://raw.githubusercontent.com/kalidke/ship-of-tools/main/scripts/install.sh \
  | bash -s -- --backend <ssh-alias>     # FE on this Mac -> BE on a Linux box
  # or: --local (everything on the Mac) / --be-only (headless Mac backend)
```

`--backend` writes a `sot-launch` that sets `SOT_HOST=<ssh-alias>` and runs the launcher, which syncs the hub's
`hosts.toml` and dials from `sotd topology plan` (the frontend spawns its own ssh child: no port, no forward). It
needs the backend installed and enrolled first, with the hub's `hosts.toml` listing this Mac (§2.1). `--local` and
`--be-only` are not supported for agent sessions on a Mac backend; use `--backend`. If the human prefers manual
steps: download `sot-<ver>-macos-aarch64.tar.gz` + `SHA256SUMS`, verify
(`shasum -a 256 -c`), `xattr -d com.apple.quarantine ./sot ./sotd ./sot-capsule`,
`./sot --dial <name>=ssh:<ssh-alias>` (no systemd on macOS; launchd wiring
is roadmap).

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
first is encouraged: download `.../ship-of-tools/<tag>/scripts/install.sh` for
the tag you will install, read it (a long commented bash script, everything under `$HOME`, no sudo,
checksums verified before use), then run `SOT_INSTALL_TAG=<tag> bash install.sh <flags>`. Run without
`SOT_INSTALL_TAG`, the file instead fetches and runs the installer of the tag it resolves: the one `--version` names,
else the latest release (the newest release of any kind on a machine that runs a pre-release). Your read cannot cover
the `curl https://install.julialang.org | sh` it runs when Julia is absent.

Two flags beyond the role: `--version vX.Y.Z` pins a specific release (the
leading `v` is required — the flag is used verbatim as the tag), and
`--prefix <dir>` relocates the install from `~/.local/share/sot` (substitute
your prefix in every path below if you use it).

Idempotent; re-running is also the upgrade path, and every run ends in one
`PASS`, `ONE STEP LEFT` or `FAIL` line (see "Update this computer").
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
pass before you report success — checks 3 and 4 are the difference between a
working install and one that can never update itself.

```powershell
# 1. The frontend runs and reports a release version (no `+` in it: a `+<sha>`
#    or `+src` version is a source build, which never self-updates).
& "$env:LOCALAPPDATA\sot\bin\sot.exe" --version

# 2. The shortcut exists and carries the Ship of Tools icon, not PowerShell's.
$sc = (New-Object -ComObject WScript.Shell).CreateShortcut("$env:USERPROFILE\Desktop\Ship of Tools.lnk")
$sc.TargetPath; $sc.Arguments; $sc.IconLocation   # IconLocation must end in logo.ico,0
Test-Path ($sc.IconLocation -replace ',0$')       # must be True

# 3. The install manifest exists — WITHOUT it the frontend never checks for
#    updates (it bails at its "not a release install" guard).
Get-Content "$env:LOCALAPPDATA\sot\install.json"  # carries schema, prefix, version, tag, hub

# 4. The update applier is present and loads under Windows PowerShell 5.1.
$e = $null
[void][System.Management.Automation.Language.Parser]::ParseFile(
    (Resolve-Path .\scripts\sot-apply.ps1), [ref]$null, [ref]$e)
$e.Count   # must be 0

# 5. The plan resolves a dial for the hub — there is no port or tunnel to
#    check: reaching it is an ssh child the frontend spawns itself.
(& "$env:LOCALAPPDATA\sot\bin\sotd.exe" topology plan |
         Select-String '^dial ') -ne $null   # must be True
```

Then launch from the desktop shortcut (not `sot.exe` directly — that has no launcher). A native window should
open and connect. Confirm the **bottom border line** shows
`fe <version> · be <version>`: dark gray means the two halves agree, **yellow
means the frontend and backend are on different builds** and the backend
needs updating. Tell the human: **press Ctrl+? for the focused pane's shortcuts, F1 for the Help drawer.**

If step 3 printed nothing, run `powershell -File scripts\install-manifest.ps1
-Hub <hub-ssh-alias>` and re-check. If its version has a `+` in it, that is a source install: it
updates via the launcher's git pull + cargo rebuild instead, which is correct
and needs no manifest.

### Linux / macOS

```bash
~/.local/share/sot/bin/sotd --version        # must print the release version
git -C ~/.local/share/sot/repo/current describe --tags   # must equal the tag
cat ~/.local/share/sot/install.json          # schema 1; version and tag equal the release
```

For frontend roles with `--backend`, also check that the hub lists this machine: run
`~/.local/share/sot/bin/sotd topology sync --hub <backend alias>`, then `~/.local/share/sot/bin/sotd topology plan`,
which must print a `dial` line for the backend. A sync refusal (`does not list this box`), or `sot-launch` printing
`continuing with no declared hosts`, means the machine is not enrolled on the hub yet (§2.1).

Check that Julia is 1.12 or newer: `julia --version`; if it reads older, run `juliaup default 1.12`.

For backend roles, check that a coding agent exists on the host: `command -v claude || command -v codex` (the
installer only warns). Tell the human to log in to it there.

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
version>`). The probe goes through `sotd stdio-bridge`, which connects only
to a socket this OS account listens on; its input stays open for the five
seconds the probe waits, because the bridge closes the connection when its
input ends:

```bash
sock="$(~/.local/share/sot/bin/sotd session-socket-path sot)"
# Ask the binary what wire protocol it speaks -- don't hard-code the
# number here (the `protocol <N>` at the end of `sotd --version`'s
# parenthetical exists exactly so out-of-tree probes like this one never
# have to; see version_line's doc comment in rust/protocol/src/version.rs).
# Omit the field when the version line prints none; the backend defaults it.
proto="$(~/.local/share/sot/bin/sotd --version | grep -oE 'protocol [0-9]+' | grep -oE '[0-9]+' || true)"
proto_field=""
[ -n "$proto" ] && proto_field="\"protocol\":$proto,"
tmp="$(mktemp "${TMPDIR:-/tmp}/sot-hello.XXXXXX")"
(
  { printf '{"v":1,"id":1,"kind":"req","op":"hello","payload":{"client_id":"install-check","last_seen_revision":0,%s"app_version":"agent-install","host":"install-check","os_user":"uid:%s"}}\n' \
      "$proto_field" "$(id -u)"; sleep 5; } \
    | ~/.local/share/sot/bin/sotd stdio-bridge --endpoint "unix:$sock" > "$tmp"
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
window should open and connect. Tell the human: **press Ctrl+? (Cmd+/ on
macOS) for the focused pane's shortcuts, F1 for the Help drawer.**

## 5. Troubleshooting (the known failure modes)

| Symptom | Cause → fix |
|---------|-------------|
| `glibc >= 2.35` error | distro too old for the prebuilt frontend → use `--be-only` here + frontend elsewhere, or build from source |
| checksum verification FAILED | truncated download → re-run; still failing = report, don't bypass |
| `topology sync failed: hub's hosts.toml does not list this box; add [host.<me>] on the hub`, or `<host> is not a listed host` from `sotd topology plan` | the hub does not know this machine → on the hub add `[host.<this box>]` with `frontend = true` (`docs/ENROLLING-A-HOST.md`), then relaunch |
| dirty-checkout refusal on upgrade | the human edited `repo/current` → `git -C ... stash` (or commit), re-run |
| ssh to `<host>` fails | read its own stderr line in the host row (the ssh child's own complaint, e.g. a refused key, is the diagnosis) |
| `topology sync failed: no hosts.toml ... yet: pass --hub <alias>` | the box never had its first sync (fresh install without `-Hub`, or migrated off a checkout whose `.sot\hosts.toml` is no longer read) → `& "$env:LOCALAPPDATA\sot\bin\sotd.exe" topology sync --hub <alias>` (or re-run `install-shortcut.ps1 -Hub <alias>`); on Linux/macOS `~/.local/share/sot/bin/sotd topology sync --hub <alias>`; then relaunch |
| backend socket missing | failed daemon start → `systemctl --user status sotd`, restart it |
| Julia instantiate fails "project and manifest are out of sync" (often naming a stdlib, e.g. `Sockets`) | stale `Manifest.toml` from a previous install left in the checkout's env dirs → manual fix: `rm ~/.local/share/sot/repo/current/julia/{kernel,repl,pluto}/Manifest.toml` and re-run |
| Julia instantiate slow on first run | normal (precompilation); minutes, once |
| session rows die / capsule pane blinks "supervisor lane not answering" forever, journal claims a start that produced no process | `$HOME` or the state dir is on a remote filesystem (NFS/CIFS/SMB2/9p/FUSE) — capsule records need a local disk (ADR 0043 decision 23) → set `XDG_STATE_HOME` to a local-disk directory (`systemctl --user set-environment XDG_STATE_HOME=/path`, or export it in `~/.bashrc`, which the unit sources) and restart `sotd` |

## 6. After the install

- The checkout **is the manual**: point yourself (and the human) at
  `~/.local/share/sot/repo/current/docs/USING.md` — on Windows that is
  `docs\USING.md` under `%LOCALAPPDATA%\sot\repo\current` (made by §2b). Inside the app, the
  terminal-drawer agent gets the right path via `$SOT_MANUAL` on every
  platform.
- Updating later:
  - **Linux / macOS** — "Update this computer" at the top of this page: one
    command, one verdict line. The app also notifies about new releases and
    stages fresh binaries itself; `sot-launch` applies them at the next start
    via `sot-apply`.
  - **Windows** — "Update this computer (Windows)" at the top of this page: one command, one verdict line. The
    frontend also checks for releases at startup and stages them (this needs `install.json`); `launch-sot.ps1` calls
    `scripts\sot-apply.ps1` on the next launch to verify and swap the binaries, keeping `.prev` and rolling back if the
    new build crash-loops. Scripts and config update the same way: the shortcut targets
    `%LOCALAPPDATA%\sot\repo\current\scripts\launch-sot.ps1` and `sot-apply.ps1`'s junction flip carries them in the same
    transaction as the binaries (docs/adr/0030-versioning-release-and-auto-update.md's 2026-09-17 amendment).
  - A source build (a `+` in its version) never self-updates on any platform;
    they update by pulling and rebuilding.
- Uninstall: `rm -rf ~/.local/share/sot ~/.config/sot ~/.local/bin/sot-launch`;
  if a unit was installed, `systemctl --user stop sotd` and remove
  `~/.config/systemd/user/sotd.service`, its folder `sotd.service.d`, `~/.config/systemd/user/default.target.d/sotd.conf`
  and any `~/.config/systemd/user/default.target.wants/sotd.service` link a pre-0.6.6 install left (on a shared home
  this uninstalls every host); also remove the desktop entry
  (`~/.local/share/applications/ship-of-tools.desktop`, plus
  `~/.local/share/icons/hicolor/256x256/apps/ship-of-tools.png`) or the macOS
  app (`~/Applications/Ship of Tools.app`).
  On **Windows**: `Remove-Item -Recurse "$env:LOCALAPPDATA\sot"` (binaries,
  install.json, updates, logs), delete `"$env:APPDATA\sot"` if you wrote a
  config there, remove `"$env:USERPROFILE\Desktop\Ship of Tools.lnk"` and
  unpin the taskbar shortcut, then delete the clone. Agent comm resources written by
  `ShipTools.update_comm()` (`~/.sot-comm`, skills under `~/.claude` /
  `$CODEX_HOME` (default `~/.codex`), the Codex plugin and marketplace under `~/.agents/plugins/`, and the launchers
  `ccb`, `ccx`, `sot-fe`, `show-result` and `sot-gh-auth` in `~/.local/bin`) are shared with other checkouts —
  remove them only if this was the machine's only Ship of Tools install. Before removing `~/.sot-comm`, take the
  four comm hook entries (commands under `$HOME/.sot-comm/bin/comm-status-*.sh`) out of `~/.claude/settings.json` and
  each `~/.claude-auth/*/settings.json`, or Claude Code keeps running hooks whose script is gone.
