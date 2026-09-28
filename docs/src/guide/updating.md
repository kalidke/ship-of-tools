# [Updating and rollback](@id updating)

Updating a release install is one command. On a `--local` install and on
Windows, an update whose frontend crash-loops right after applying is rolled
back automatically.

Two mechanisms complement rather than compete (release installs only —
**source builds are stamped `-dev` and never self-update**; they update by
`git pull` + rebuild):

| Mechanism | What it does | What you do |
|-----------|--------------|-------------|
| **Built-in update check** | The backend checks GitHub for new releases (daily and on demand, plain HTTPS, no GitHub auth), **notifies the frontend**, and prepares the whole new version in the background: binaries, a tag-pinned checkout and instantiated Julia environments, verified against the release's `SHA256SUMS`. The staged version is applied at the next launch (or daemon restart) as an offline switch, with the previous version kept for rollback. | Nothing — you'll see the notice; relaunch when convenient. |
| **Re-running the installer** | The **complete** update in one command: new binaries *and* the repo checkout moved to the new tag *and* Julia envs re-instantiated, atomically. It is also what migrates a pre-0.6 install onto the auto-update layout. | `curl -fsSL …/install.sh \| bash -s -- <your-role>` (same command as install; add `--version vX.Y.Z` to pin). |

**When in doubt: re-run the installer.** It is idempotent and moves
everything together. With an agent, say:

```text
Update Ship of Tools: fetch https://raw.githubusercontent.com/kalidke/ship-of-tools/main/docs/INSTALL-AGENT.md and follow it. If this machine runs a source build, replace it with a release install.
```

`SOT_UPDATE_MODE` controls the built-in check: `notify` (the default) stages
and applies at the next launch, `off` disables it, and `auto` additionally
restarts the backend to apply once no frontend is attached. That restarts
REPLs and kernels, so their in-memory state is lost, which is why it is
opt-in; agent sessions survive it.

On **Windows** the frontend runs the same check itself, since there is no
local backend to do it, and `launch-sot.ps1` applies the staged update at the
next launch through `scripts\sot-apply.ps1`: the same verification and
rollback, and a new build that crashes within 10 seconds of its first launch
is reverted automatically.

### Close the window before you launch it again

The launcher allows **one launcher per user**. Clicking the shortcut while
Ship of Tools is already running does not open a second window — the second
launcher sees the first one's lock and exits. What it also does not do is run
the pending-update step, because that step comes much later in the same
launcher it just exited from.

Two things that click can look like, and neither of them is a fault:

- **The window is already up.** The second launcher exits immediately and
  **silently** — no message, no window.
- **The first launcher is still starting up or rebuilding.** The second one
  waits for it, for up to 15 minutes, before giving up with a message box.
  Wait for the window rather than clicking again.

So on Windows a staged update applies at the next launch **from a closed
window**, not at the next click. Quit Ship of Tools, then start it from the
shortcut. Closing it loses nothing on the backend: sessions, agents and REPLs
run under the backend's own supervisors, not under any frontend.

### The file watcher no longer holds its own staging area

Staging unpacks into a temporary directory inside the staging area and renames
it to its final name there; applying it to the install is a separate step at
the next launch. Windows refuses to rename a directory while anything holds a
handle on it, and the file watcher that keeps previews fresh was watching the
staging area along with everything else — so on some machines an update would
stage, fail to commit, and stage again on the next check, indefinitely.

The watcher now excludes the update staging root, asking the updater where
that root is rather than guessing at it.

<!-- SCREENSHOT-SLOT: name=update-notice
     shows: the frontend's update notice, on a build that carries the 0.6.6
            strip spacing
     shoot with: scripts/docs-media.sh stills update-notice
     blocked on: owner sign-off on the 0.6.6 strip spacing and badge graphics -->

On update the installer swaps binaries while keeping `.prev` copies, fetches
tags in `$PREFIX/repo/current`, checks out the requested tag, **refuses to
move a dirty checkout** (commit/stash/revert first — the checkout is
read-only by convention), and verifies `HEAD` equals the tag's recorded
commit, so a moved tag or half-checkout fails at update time, not at first
use.

## Pinning and going back

- **Pin a release.** Add `--version vX.Y.Z` to the install command to move to
  (or stay on) a specific tag; the same flag moves you *back* to an older tag.
- **Automatic rollback.** Applying a staged update is an offline pointer flip
  done by `sot-apply`, which records the previous install as *last good*.
  The `--local` launcher runs `sot-apply --rollback` if the frontend exits with an error
  within 10 seconds twice in the 30 minutes after an update; Windows does the
  same through `launch-sot.ps1`. A `--backend` (frontend-only) install on
  Linux or macOS does not roll back on its own: re-run the installer with
  `--version` to go back.
- **Keep the checkout clean.** The installed checkout under
  `$PREFIX/repo/current` is the manual and the resource tree. Updates refuse to
  move a dirty tree, so leave it read-only.
