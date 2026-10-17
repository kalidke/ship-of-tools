# A second frontend

The backend owns the project state — sessions, kernels, file watching — and the
frontend renders what it is sent. So a desktop and a laptop can attach to the
**same backend at once** and see the same sessions, the same work-state
colours and the same REPLs.

## Add one

The hub's `~/.config/sot/hosts.toml` must list the second machine (`[host.<box>]` with `frontend = true`) before its
install, or its window opens with no host. Then install the frontend role on that machine against the same backend SSH
alias, with `install.sh --backend <ssh-alias>`; on Windows, extract all three binaries from the release zip and run
`scripts\install-shortcut.ps1 -Hub <hub-ssh-alias>`. The commands are in [Going remote](../start/remote.md) and
[Install](../start/install.md). Launch it with `sot-launch`; it spawns its own `ssh` child and connects as one more
client of the daemon.

## What is shared and what is not

| Shared (lives on the backend) | Per frontend |
|-------------------------------|--------------|
| sessions and their agents | window layout and which pane has focus |
| each workspace's Julia kernel and REPL | the active session and cursor positions |
| work-state colours and comm messages | the Terminal drawer (a shell on *that* machine) |
| files and previews | `settings.toml` (layout preset, GPU preference) |

The agents run under the backend's own supervisors, not under any frontend, so closing a frontend on another computer
than the backend ends nothing. Closing the last window on the computer that runs the backend ends its sessions, including
any that the other frontend was viewing; see [What keeps running](../start/running.md).

## [Where a session's output lands](@id browser-window)

With two windows attached, a file an agent shows (`sot-fe preview`, `show-result`) reaches the preview pane and the
badge of **every** window. A browser an agent opens (a docs site, a URL, a `wglshow` figure) opens on **one** computer:
the window you used last, however long ago; with none used yet, the one that connected last. A window counts as used
from its second key press, click or wheel step within five minutes, and a switch between windows is noticed within
about six seconds. Name a window with `--fe <host>` to override this. A window that is reconnecting at that moment is
not chosen, and a browser open cannot wait for it.

## Many machines, one workspace

To make a server reachable from every frontend in a fleet — not only the one
you installed it from — enrol it on the hub: see
[Enrolling a host](enrolling.md).
