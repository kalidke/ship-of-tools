# ADR 0024: Open backend web pages in the local browser (dynamic port-forward)

**Status:** Accepted (2026-06-20; built + merged same day)
**Date:** 2026-06-20

## Context

The frontend can already hand a URL to the OS default browser
(`open_url_in_browser`, gpu.rs:698 — `cmd /c start "" <url>` on Windows,
`xdg-open`/`open` elsewhere). Two flows use it to open a **live backend web
server** in the user's real browser, with full CSS/JS/sub-page fidelity:

- **Pluto** — `pluto.open` (backend) returns `URL http://127.0.0.1:1234/edit?...`;
  the FE opens it (gpu.rs:7589). Reachability relies on a **static** SSH forward
  `-L 1234:127.0.0.1:1234` baked into the launcher tunnel
  (launch-devenv.ps1:253).
- **Video** — same shape on port 1235 (ADR 0018; launch-devenv.ps1:256).

So "forward a BE port → open the live URL locally" is a **proven pattern** — it
just hangs off two hardcoded ports. What's missing is the general case the user
asked for: open **any** backend web page on **any** port (a Documenter preview,
a Genie/HTTP.jl app, a notebook server, a coverage report, …) from a keybind,
with sub-pages and assets intact.

`o` ("open") is the closest existing affordance, but for HTML it writes a single
static file to temp and opens *that* (`open_html_in_browser`, gpu.rs:731) — no
server, so relative CSS/JS and `<a>` sub-pages 404. That is the gap.

### Why not "the VS Code way" (single connection, multiplexed forwards)

VS Code adds forwards as extra channels over its **one** existing SSH connection
— no `ssh -L` per port. With OpenSSH that is `ControlMaster` + `ssh -O forward`.
**This does not work on Windows**, which is the FE's primary platform. Tested
live on this machine against the dev backend (2026-06-20):

| ssh binary | `ssh -V` | ControlMaster + `-O forward` |
|---|---|---|
| Windows system ssh (what the PowerShell launcher uses) | `OpenSSH_for_Windows_9.5p2` | **FAILS** — `getsockname failed: Not a socket`, master never starts; `-O forward` silently falls through to a plain login shell instead of adding a forward |
| git-bash MSYS2 ssh | `OpenSSH_9.6p1, OpenSSL` | works (emulated Unix sockets) |

Win32-OpenSSH lacks the Unix-domain-socket fd-passing ControlMaster needs. VS
Code sidesteps this entirely by implementing its **own** SSH multiplexer in its
Node `ssh2` library, never relying on OpenSSH multiplexing. git-bash's ssh works
but is an **undeclared dependency** and a **different binary than the launcher's**
— relying on it would split the FE's ssh from the tunnel's and break on any
machine without git-bash on PATH.

## Decision

**Cross-OS is a hard requirement** (FE runs on Windows, Ubuntu, macOS). That
single fact selects the design: the FE must **not** spawn its own `ssh` (binary
path, flags, and child-management differ per OS — and Win32-OpenSSH can't even do
ControlMaster, §"Why not the VS Code way"). Instead, reuse the mechanism the repo
**already** uses cross-OS for exactly this — a **static `-L` forward in the
per-OS launcher** (`scripts/launch-devenv.ps1` and `scripts/launch-devenv.sh`),
the same way Pluto (1234) and video (1235) are forwarded — plus a backend
`*.open` op that returns a `http://127.0.0.1:<port>/…` URL, opened by the
existing cross-OS `open_url_in_browser` (`cmd start` / `xdg-open` / `open`).

The feature opens **any on-disk static site/page** in the browser — a Documenter
`docs/build`, an example project's `__site`, a built coverage/report tree, a standalone
`index.html`. Documenter was the first cut; the design below is the generalized
form (revised after dogfooding — see "Revision: generalize" at the end).

### 1. Backend — a static **site** server rooted at the *selected* directory

The existing `http_serve.rs` is single-file and video-gated, so it cannot serve a
site (a tree of HTML/CSS/JS/assets + sub-paths). Add a sibling **static directory
server** (new `site_serve.rs`) that:

- Binds `127.0.0.1:<site_port>` (env `SOT_DOCS_PORT`, default **1236** — env
  name kept for launcher compatibility), spawned at backend boot next to the
  video server.
- Serves files under a **current site root** that the `docs.open` handler sets on
  each open (a `RwLock<Option<PathBuf>>`), refusing anything that canonicalizes
  outside it (path-traversal guard). One active site at a time — last-opened wins.
- Resolves directory requests (`/`, `/foo/`) to `index.html`.
- Sets web content-types (html, css, js, mjs, json, svg, png, jpg, gif, webp,
  woff, woff2, ttf, ico, map, wasm, pdf, …).

Serving from the **true site root** (URL path = path relative to that root) is
what makes **both** link styles work — root-relative (`/assets/…`, what an
example project's `__site` emits) and page-relative (`../assets/…`, Documenter local) resolve correctly only
when served from the site root. (`o`'s file:// open breaks search/JS for exactly
this reason.) The root is the **cursored file's own directory**, so the feature is
**workspace-agnostic**: the cursored path is absolute, so `W` opens whatever is
selected wherever you are — no dependence on the daemon's launch project.

### 2. Trigger — new FE keybind `W` ("web") → `docs.open`

`W` sends a `docs.open` request (op name legacy from the Documenter cut; it now
opens any site) carrying the cursored file's absolute backend path. The handler
derives `(site_root, url_path)` so both link styles resolve:

- cursor on a **directory** → serve it, open `/` (its `index.html`; error if
  none);
- cursor on **`index.html`/`index.htm`** → serve its **parent**, open `/`;
- cursor on **any other file** → serve its **parent**, open `/<filename>`.

It then `set_site_root(root)` and returns `http://127.0.0.1:<site_port>/<rel>`.
The FE opens it with `open_url_in_browser` (already cross-OS). `W` is otherwise
unbound (keybindings.toml has only `pane.*`); `o`/`O` are untouched.

### 3. Forwarding — static `-L <docs_port>` in BOTH launchers

Add `-L 1236:127.0.0.1:1236` (env-overridable `SOT_DOCS_PORT`) to:

- `scripts/launch-devenv.ps1` `$sshArgs` (next to the pluto/video `-L` lines), and
- `scripts/launch-devenv.sh` (next to `-L 1234 -L 1235`).

Cross-OS by construction: each OS's launcher already owns its tunnel; we add one
line to each. The **local-host backend** case (no SSH) needs no forward — the URL
is already `127.0.0.1:<docs_port>` on the same machine.

### Full fidelity

Serving the site root over the forwarded port and opening
`http://127.0.0.1:1236/` resolves every asset and `<a href>` against the same
origin; any in-page JS/search/WebSocket rides the same loopback (Pluto already
proves WS over `-L 1234`). No HTML rewriting, no proxy.

### Deferred to a generalization phase

The original sketch (open **any** BE port on **any** node via node-payload
`http_port` discovery + a **dynamic** per-port forward) is deferred. It needs
either dynamic forwarding (the cross-OS-fragile FE-spawns-ssh path, or teaching
the launcher to add forwards live) or a backend HTTP proxy multiplexed over the
existing daemon socket (no new ports, no new ssh — the most promising cross-OS
route, but a larger build). The `docs.open` op returns the same `(port, path)`
shape a node payload would carry, so promoting this to payload-driven discovery
later is mechanical.

## Consequences

- Cross-OS by construction: the only per-OS code is one `-L` line added to each
  launcher (`.ps1` + `.sh`); the FE never spawns ssh, and `open_url_in_browser`
  already branches per OS. Nothing new is Windows-specific.
- Reuses the proven video/pluto path end to end (boot-spawned loopback server →
  static launcher forward → `*.open` op → browser), so the risk surface is small
  and familiar.
- The docs port (1236) is forwarded for every session whether or not docs are
  built — harmless (nothing listens until `docs/build` exists and the server
  serves it), matching how 1234/1235 are always forwarded.
- A new launcher `-L` line only takes effect after the tunnel is (re)established,
  so an already-open reused tunnel (`launch-devenv.sh` reuse path) won't forward
  1236 until the next fresh tunnel. Acceptable; documented.

## Alternatives rejected

1. **FE spawns dynamic `ssh -N -L` per port.** The original sketch. Rejected once
   cross-OS became a hard requirement: ssh binary/flags/child-management differ
   across Windows/Ubuntu/macOS, and Win32-OpenSSH can't do ControlMaster (tested).
   The launcher already solves tunneling per-OS — don't duplicate it in the FE.
2. **ControlMaster single-connection multiplexing (OpenSSH "VS Code way").**
   Fails on Win32-OpenSSH (tested above). git-bash ssh works but is an undeclared
   dependency and a different binary than the launcher's. Rejected.
3. **Pre-forward a port range** (e.g. 8000–8099) in the launchers. Wastes
   forwards and caps which ports work. Rejected for the fixed-port Documenter cut.
4. **Proxy HTTP over the existing daemon socket** (tunnel request/response frames
   through the protocol — no new port, no ssh). The most promising route for the
   *general* "any port" case and genuinely cross-OS, but a larger build than a
   fixed-port static forward needs. **Deferred** to the generalization phase.

## Implementation sketch (this build)

- **Protocol (`ops.rs`):** add `op::DOCS_OPEN = "docs.open"` + `DocsOpenReq{path}`
  / `DocsOpenRes{url}`, mirroring the video/pluto structs.
- **Backend (`site_serve.rs`, new):** rooted static dir server (content-types,
  index resolution, traversal guard). `server.rs`: spawn it at boot next to the
  video server; route `op::DOCS_OPEN` to a new `handlers::handle_docs_open` that
  validates `docs/build/index.html` and builds the URL.
- **Frontend (`transport.rs`):** `OutgoingReq::DocsOpen{path}`,
  `IncomingEvt::DocsOpened{result}`, `PendingKind::DocsOpen`, and the
  serialize/deserialize arms. **`gpu.rs`:** `W` key → `DocsOpen`; `DocsOpened`
  event → `open_url_in_browser`.
- **Launchers:** add `-L 1236:127.0.0.1:1236` (env `SOT_DOCS_PORT`) to both
  `launch-devenv.ps1` and `launch-devenv.sh`.
- **No node/kernel change** for this cut — `docs.open` is project-level. The
  node-payload `http_port` generalization is the deferred phase.

## Revision: generalize from "Documenter docs" to "any static site" (2026-06-21)

Dogfooding the first cut surfaced the real requirement: rooting at the daemon's
`<launch-project>/docs/build` meant `W` always opened **DevEnv's** docs even when
the FE was in a *different* workspace (e.g. an example lab's project site) — and the
ask was never "each package's docs" but "open **anything** on disk, typically an
`index.html`." Fix (backend-only — the FE already sends the cursored absolute
path, so no FE change, no relaunch, no launcher change):

- `site_serve` holds a **mutable current site root** (`RwLock<Option<PathBuf>>`,
  `set_site_root`) instead of a boot-fixed `docs/build`; `spawn` no longer takes a
  project root.
- `handle_docs_open` roots at the **cursored file's own directory** (rule in §2
  above) and `set_site_root`s it. Workspace-agnostic; no `workspace_id` needed
  (the absolute path already pins the location).
- Documenter is now just one instance (cursor `docs/build/index.html`); an
  example project's `__site/index.html`, report trees, and standalone pages all work the same way.

Trade-off accepted: **one active site at a time** (last-opened re-points the
root; a stale prior tab 404s on reload). A path-prefixed multi-site scheme is a
later refinement — and since root-relative `/asset` links force one-site-per-
origin anyway, multi-site really needs the deferred per-port/proxy work. The op
name stays `docs.open` (legacy) to keep the change backend-only.
