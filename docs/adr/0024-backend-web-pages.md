# ADR 0024: Open backend web pages in the local browser (dynamic port-forward)

**Status:** partly superseded by ADR 0035 (2026-06-20; built + merged same day)
**Date:** 2026-06-20

> **Transport note (2026-08-10, updated 2026-09-29):** the static `-L` helper
> forwards described here are the pre-ADR-0035 transport. Since v0.5.0 these
> pages ride the daemon proxy over the control connection; the fixed forwards
> were retired first as an opt-in fallback (`SOT_LEGACY_FORWARDS=1`) and then,
> once the control connection itself stopped being a tunnel (the isolation
> lane, `dev/output/isolation-plan.md`), deleted outright — there is no
> forward left to opt into. The open-in-browser flow this ADR decides is
> unchanged.

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

## Revision: linked data through tracked links (v0.6.6)

A page can show data that lives outside its repo, through a symlink the repo
tracks. Two roots, once one, are now separate. The **content root** is where
ordinary files are served from (`find_site_root`). The **URL root** is
where the URL path space maps: the repo top when the page rides the shared prefix
server and sits in a git repo, else the content root. A browser
collapses `../../../data/x.mp4` above the nonce segment, so the link folder is
reachable only when the URL space starts at a common ancestor. The URL space
widens in every repo, so a page's link is served or refused with words, never
answered that no site is open. Root-relative sites keep their own pool origin and are not
widened. Widening the URL space does not widen what is served.

Follow rule, checked on every request:

- `.git` in any path part is refused, and so is a `..` part. A resolved path
  with a `.git` component below the folder it is served from (the site or the
  mount folder) is refused too (a tracked link into `.git`, an 8.3 short name): no
  file whose resolved path lies inside a `.git` directory below that folder is
  served.
- A path that resolves inside the content root is served as before.
- Otherwise the first symlink below the URL root must be tracked by git (the set
  is rebuilt whenever the repo index file changes), and the file must resolve to
  a path under a mount folder (`/mnt`, `/media` or `/Volumes`), outside the home
  folder and the repository, with no part below the mount folder whose name
  starts with `.`; and the link itself must point there too: its target
  must be written as its absolute real path: absolute, with no `.` or `..` part,
  and equal to its canonical path once a leading mount folder as written is
  mapped to its canonical path. So no symlink lies on its way and no name of a
  private folder can be walked into and left again (a route through the home
  folder, another link or a `..` is refused whatever it ends on; a relative
  target is refused outright). That canonical target is
  judged by the same bounds before the request is resolved, lies outside the
  repository, and names a data folder: a link to `/`, or to a folder holding the home folder or the
  repository, is never followed. No file lists data folders.
  A file on a dead hard-mounted share hangs the request that resolves it, as
  reading it would.
- Each refusal says why. A linked path that does not serve (the link's target or
  the file outside the bounds, missing, private, hidden, unreadable or not a
  regular file) gets one answer, naming the path as asked; the daemon's log
  says which.
- Off Linux and macOS no link is followed.

The file is opened without following a symlink below its base (`openat` with
`O_NOFOLLOW`), so a link swapped in after the check fails the open instead of
escaping. What a page's script can read: the content root, plus, through a
tracked link, files under a mount folder outside the home folder and hidden
folders; never anything inside a `.git` directory below the folder it is served
from, never an untracked repo file outside the content root, never anything
through an untracked link.
Outside the content root no answer depends on whether the path exists: a path that reaches no link gets one refusal
whether or not it exists, so a script learns the names of links under the URL root and nothing else about the repo's
files; and a linked path the bounds refuse gets one refusal whether or not it exists, so a link learns nothing about
what exists in a private place. The time to answer still differs slightly (an existing path costs a few `lstat`s
more); that is a known limit.
On a Mac without the Command Line Tools git is never run, no link is followed, and a link is refused with words that
say git is needed.
Range, content types and the empty-file case are shared with the video server.

## Amendment 2026-10-11: Quarto render diagnostics (QUARTO-ERROR)

A failed `quarto.open` keeps its complete diagnostic message within 65,536 UTF-8 bytes. Reserve applicable transport guidance, the first ERROR occurrence and final ten stderr occurrences; retain further ERROR occurrences in source order while they fit, then report one `[N more ERROR lines omitted]` line when needed. Count omitted ERROR occurrences, not bytes; overlapping retained occurrences appear once. Mandatory lines are complete when they fit; if they alone exceed the budget, clip only the oversized lines, each as an explicitly marked UTF-8-safe excerpt; the shorter mandatory lines stay whole.

Match the transport newline error in full decoded stderr. Its hint survives an omitted triggering occurrence, including an all-blank selected tail. It explains incomplete Julia server transport publication: Quarto 1.9.38 and later wait up to two seconds for the newline, slow server startup can still fail, and upgrading may help when using an older release.

The bounded response fits the existing envelope cap and preserves `quarto_render_failed` and its wire shape. The daemon captures full stderr and attempts the render once.

## Amendment 2026-10-16: a page is served from the site it belongs to (PROJLOG-STYLE)

`docs.open` serves the cursored file, or the cursored folder's `index.html`, from the site it belongs to: the outermost
folder holding an `index.html` between it and the top of its repository (the nearest folder holding `.git`) or the
workspace root, whichever is nearer, and opens it at its place below that folder. With no index on the way, the file's
own folder is served, as before.

Before this, the walk took the innermost folder holding an index and climbed on only while each parent held one, and an
`index.html` or a folder was served from its own folder. A project log is one site with index pages inside it
(`journal/index.html`, each campaign's `experiments/<campaign>/index.html`) and a folder with none between them and the
log's front page (`experiments/`). Its campaign pages, and its journal and campaign index pages opened directly, were
served from their own folders and lost the log's `assets/`: its CSS, KaTeX and `site.js`. A Documenter page built with
pretty URLs (`man/intro/index.html`) and a nested index page of a root-relative site (`__site/blog/index.html`) lost
theirs the same way.

The site is everything a page's script may read, so the walk has two bounds:
- It ends at the top of the page's repository. Before this, the first index above a page could lie outside its
  repository, in a folder of several repositories, and the page could read the other repositories' files.
- No site root holds the home directory, since its pages could read the ssh keys and the daemon's own token: the
  walk stops below it, and a page whose own folder holds it is refused. Before this, a page directly in the home
  directory, or with no index between it and an `index.html` there, was served from the home directory.

The cost: an index page in any folder above a page, up to the top of its repository (a source tree's `index.html` above
its build output, or `web/index.html` above `web/dist/`), or up to a workspace root outside any repository, now takes in
every page below it, and those pages may read every file under it, untracked ones too. Before this, a folder with no
index kept a nested site apart, and now a build's `dist/index.html` directly below a source `index.html` is taken in
with no folder between. Two measured cases: a Vite build with base `./` reads the source tree's `.env.local` through
`../.env.local`, and a page outside any repository under an index page at the workspace root reads another repository's
`.env`; both were refused before. On a workspace that is the home folder, the walk reaches the folder below home. To
keep a nested site apart, move or rename the outer index page. A page with root-relative links has its own origin and is narrowed, not widened: it is rooted at the innermost folder
on the walk that holds the file its first plain link names (a Vite build's `dist/`, a Jekyll `_site/`), taking the links
in document order and skipping any that names no file there, else at the walk's root like any page. A link is plain
when every part is a name (no `..`, drive, `\` or `:`), and a folder counts only when the link, joined to the folder,
names a regular file with no symlink anywhere on its path. So a link through a symlink never narrows, the page's URL,
which its script can read, shows nothing about files outside the walk's root and confirms no guess at the page's own
absolute path, and at most 64 distinct links are tried, which bounds the open. A rule rooting every page at the innermost folder
that holds the files it loads would narrow the cases above; it is not taken, since a page that names a file at the walk's
root still takes in everything under it.

The repository top is the nearest folder holding `.git`, a folder or a worktree's file. A checkout with none (an extracted
archive, a Mercurial checkout) is bounded by the workspace root, or on a home-folder workspace by the folder below home.

## Amendment 2026-10-16: linked data with no list (LINKS-NO-LIST)

A symlink a repository tracks serves with no list of data folders to keep: the `data-roots` file is no longer read.
A tracked link serves a file when the file's canonical path lies under a mount folder (`/mnt`, `/media`, `/Volumes`),
outside the home folder, with no part below the mount folder whose name starts with `.`, through a link to neither
`/` nor a folder holding the home folder or the repository (the follow rule above; `Bounds` in
`rust/backend/src/pages/site/links.rs`). Only a link git tracks is followed. The link's own target, written as its absolute real path (as `ln -s /mnt/<share>/...` writes it), is
judged first, by the bounds the file meets and as not too wide (not `/`, nor a folder holding the home folder or the repository), and the repository's own files serve only from
the site folder. Every refusal, of the link or of the file, is R3's one answer, so a link that names everything
(`/`, `~`, `..` from a repo on a share) serves nothing and says nothing about where the home folder or the
repository is, or whether a guessed path exists.

The rule allows where the operating systems mount shares and drives, rather than denying a list of private places.
An allow rule fails closed: a link anywhere else is refused with words that name the mount folders. A deny list fails
open at every place it forgets (the Julia depot, temporary folders, `/proc`, other accounts' homes, each platform's
own), and keeping it current is the list this replaces.

What a page's script can read through a link its repository commits, and so what a repository cloned from someone
else can read: every file the account can read under the mount folders, outside the home folder and hidden folders.
That is every share mounted on the computer, other groups' shares included; on WSL, the Windows drives under `/mnt`;
the Windows install's own sot folder there (`AppData/Local/sot` under the Windows profile); and a copy of a home
folder kept on a mounted volume, outside its hidden folders. Before this, a link reached only
what lay under a root the computer's `data-roots` file declared, and nothing where no file existed. A home folder
mounted a second time under a mount folder (an NFS server's own export path, a bind mount) is not recognized as the
home folder; its hidden folders stay refused. A symlink someone else placed on a share, below a link's target, is judged
only by the file it reaches, so a route that enters the home folder by such a symlink and leaves it by another
serves; writing to a share is what that takes. Only a regular file on `/proc` or `/sys` is refused by filesystem type (a root mounted under a
mount folder with `/proc` bound inside it); a root or a filesystem bind-mounted under a mount folder is otherwise read
as files there, other kernel filesystems' included.

Removed with the file: the refusal when no root is declared (R2c); the refusal of a link inside the linked folder
that leads out of it (R4), since the bounds now judge the file a link reaches, wherever an inner link leads; and the
separate answer for a link that does not resolve (R5), now R3's one answer, which names the path as asked, as R2a
does, so it never shows whether a folder lies behind it.
