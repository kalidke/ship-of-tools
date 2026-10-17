# [Updating and rollback](@id updating)

Updating a release install on Linux or macOS is one command. Tell a coding agent running on the machine

```text
update sot from the repo
```

or run it yourself:

```bash
bash ~/.local/share/sot/repo/current/scripts/install.sh
```

With no options it moves to the newest release on the channel the machine already runs (a stable release follows the
stable releases; a release candidate follows the candidates), keeps the role the install recorded, and never moves the
install backwards. It ends with exactly one line:

| Last line | Meaning |
|-----------|---------|
| `PASS: Ship of Tools <v> at <commit> on this computer` | Done: the binaries, the checkout, the Julia environments and the agent comm are on `<v>`. |
| `ONE STEP LEFT: <step>` | The install is whole and one thing remains that only you can do, usually to quit the window and start it again, since it still runs the old version. |
| `FAIL: <check>: <detail>` | Something did not hold. Run it again after fixing what the line names; a half-finished run is safe to repeat. |

Three `FAIL` lines are questions rather than faults:

- `daemon:` names the rows whose REPL is live. Updating restarts the backend, which ends those REPLs' variables (the rows
  and their agents keep running). Nothing was changed. Run it again with `--restart-daemon` when that is fine.
- `target:` says the machine runs a build that is not a release (a `+` in its version, a build of the line or a source
  build). Name the target with `--version vX.Y.Z`, or give `--archive <folder>` for a folder `scripts/pack-build.sh`
  packed.
- `direction:` says the newest release is older than what runs here. Add `--version vX.Y.Z` to go back on purpose.

Without a network, `--archive <folder>` installs from a folder carried to the machine (the base clone still needs the
repo's origin unless it already holds the commit). On Windows the frontend stages the update and `launch-sot.ps1` applies
it at the next launch, below.

## What updates a machine

Two mechanisms complement rather than compete (release installs only; **a source build, with a `+` in its version, never
self-updates** and updates by `git pull` and a rebuild):

| Mechanism | What it does | What you do |
|-----------|--------------|-------------|
| **Built-in update check** | The backend checks GitHub for new releases daily, **notifies the frontend**, and prepares the whole new version in the background: binaries, a tag-pinned checkout and instantiated Julia environments, verified against the release's `SHA256SUMS`. The staged version is applied at the next launch (or daemon restart) as an offline switch, with the previous version kept for rollback. | Nothing: you will see the notice; relaunch when convenient. |
| **Running the installer** | The **complete** update in one command, above: new binaries *and* the repo checkout moved to the new tag *and* Julia envs re-instantiated, with one verdict line. It is also what migrates a pre-0.6 install onto the auto-update layout. | `bash ~/.local/share/sot/repo/current/scripts/install.sh`, or say "update sot from the repo". |

`SOT_UPDATE_MODE` controls the built-in check: `notify` (the default) stages and applies at the next launch, `off`
disables it, and `auto` additionally restarts the backend to apply, if no frontend is attached when the daily check arms
an update (otherwise it applies at the next launch or restart). That restarts REPLs and kernels, so their in-memory state
is lost, which is why it is opt-in; agent sessions survive it.

The daemon of a machine runs the check; a window runs it itself only where the daemon does not own updates (a host the
topology declares `daemon = false`, or a `--backend` frontend-only install, on any OS). A pre-release install follows
pre-releases and a plain release follows plain releases; the check never moves an install backwards or into a dev
build, and an armed update records the install it was armed over, so `sot-apply` drops one that no longer fits (the
installer moved on, or the version is not above the install) without stopping the daemon. A host the hub's
`hosts.toml` names with neither `daemon` nor `frontend` runs no `sotd` and never arms into an install it shares.
`sotd update stage <folder> --prefix <prefix>`, the operator's command for a packed build, is the one way to arm the
same or an older build.

On **Linux and macOS** each release's own `sot-apply` applies the next update, and brings the agent comm (`~/.sot-comm`)
to the applied commit on every role. On **Windows** `launch-sot.ps1` applies the staged update at the next launch
through `scripts\sot-apply.ps1`: the same verification and rollback.

## What the installer does to the checkout

The installer fetches tags into `$PREFIX/repo/base`, adds a worktree for the release at `$PREFIX/repo/versions/<tag>`,
checks that its `HEAD` is the tag's recorded commit (a moved tag or a half checkout fails at update time, not at first
use), and points `$PREFIX/repo/current` at it. It refuses to go on only if that new tag's own folder has local changes.
The previous version's folder stays for rollback, and older ones are removed, except a folder with local changes: the
installer names it, keeps it, and removes it on a later run once it is clean. Do not edit the checkout; it is the manual
and the resource tree, and changes in it are not carried to the next version.

## Pinning and going back

- **Pin a release.** Add `--version vX.Y.Z` to move to (or stay on) a specific tag; the same flag moves you *back* to an
  older tag.
- **Automatic rollback.** Applying a staged update is an offline pointer flip done by `sot-apply`, which records the
  previous install as *last good*. The `--local` launcher runs `sot-apply --rollback` if the frontend exits with an error
  within 10 seconds twice in the 30 minutes after an update. Windows rolls back after a single such exit within two hours
  of the update, through `launch-sot.ps1`. A `--backend` (frontend-only) install on Linux or macOS does not roll back on
  its own: run the installer with `--version` to go back.
