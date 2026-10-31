# A second frontend

The backend owns the project state — sessions, kernels, file watching — and the
frontend renders what it is sent. So a desktop and a laptop can attach to the
**same backend at once** and see the same sessions, the same work-state
colours and the same REPLs.

## Add one

On the second machine, install the frontend role against the same backend SSH
alias:

```bash
curl -fsSL https://raw.githubusercontent.com/kalidke/ship-of-tools/main/scripts/install.sh | bash -s -- --backend myserver
```

(On Windows, use the release zip and `scripts\install-shortcut.ps1`; see [Install](../start/install.md).)
Launch it with `sot-launch`; it spawns its own `ssh` child and connects as one
more client of the daemon. Nothing on the server changes.

## What is shared and what is not

| Shared (lives on the backend) | Per frontend |
|-------------------------------|--------------|
| sessions and their agents | window layout and which pane has focus |
| each workspace's Julia kernel and REPL | the active session and cursor positions |
| work-state colours and comm messages | the Terminal drawer (a shell on *that* machine) |
| files and previews | `settings.toml` (layout preset, GPU preference) |

Closing one frontend affects nothing the other sees: the agents run under the
backend's own supervisors, not under any frontend.

## Where a session's output lands

With two windows attached, a file an agent shows (`sot-fe preview`, `show-result`) reaches the preview pane and the
badge of **every** window. A browser an agent opens (a docs site, a URL, a `wglshow` figure) opens on **one** computer:
the window you used last, however long ago; with none used yet, the one that connected last. A window counts as used
from its second key press, click or wheel step within five minutes, and a switch between windows is noticed within
about six seconds. When the window you used last is away (its computer asleep or closed, or the window reconnecting),
nothing opens on another computer: the agent is told which computer's window is away, and a question's page stays
reachable from the Queue. Another window connected from that same computer, such as a restarted window, takes it
instead. Name a window with `--fe <host>` to override this.

## Many machines, one workspace

To make a server reachable from every frontend in a fleet — not only the one
you installed it from — enrol it on the hub: see
[Enrolling a host](enrolling.md).
