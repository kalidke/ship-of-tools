# ADR 0032 — Interactive browser-served figures (WGLMakie/Bonito)

**Status: ACCEPTED — IMPLEMENTED + VALIDATED LIVE END-TO-END**
(2026-07-12, branch `feat/wglmakie-browser`).

The full path works and was confirmed by the maintainer in a real FE browser:
`wglshow(fig)` → Bonito serves the figure on `127.0.0.1:1241` → the REPL emits a
`browser` frame → the FE auto-opens the OS browser → **interactive** figure
(pan/zoom/rotate) renders over the `-L 1241` SSH tunnel. Every unknown closed:

- `browser` REPL frame kind + `BrowserView`/`browserview` + `wglshow` in
  `ShipToolsRepl`; FE opens the frame via `open_url_in_browser` (190 FE + 4
  protocol + 36 REPL tests green; `wglshow` validated headlessly — returns a
  `BrowserView`, server serves HTTP 200 — and live in the FE browser).
- Port **1241** forwarded by all three launchers (1237–1240 are the docs pool,
  not spare — see the Context correction).
- Bonito **5.1.0** API pinned (`proxy_url`, `Server(app,url,port;proxy_url)`,
  `App`, `online_url`).

## Context

CairoMakie gives us static plot artifacts today: the REPL emits an `image` frame,
the FE rasterizes it in the drawer, and `show-result` badges saved PNGs. That's
the right tool for a *result you look at*. It is the wrong tool for a *figure you
manipulate* — pan/zoom/rotate a 3D scene, hover datapoints, drive a slider.

WGLMakie is Makie's interactive backend. It renders in a browser via **Bonito**
(formerly JSServe): displaying a figure starts an HTTP server that serves an HTML
page plus a **WebSocket** carrying the interaction events. The figure lives in
the process that holds it.

We already open browser content from a backend-served loopback port and hand the
URL to the FE's OS browser — three services do exactly this:

| Service | Port | URL shape | ADR |
|---|---|---|---|
| Pluto  | 1234 | `http://127.0.0.1:1234/edit?id=…`  | — |
| Video  | 1235 | `http://127.0.0.1:1235/<abs-path>` | 0018 |
| Docs   | 1236 | `http://127.0.0.1:1236/<rel>`      | 0024 |

The launcher forwards **1234–1240** over the SSH tunnel
(`install.sh`/`launch-sot.sh`: `-L <p>:127.0.0.1:<p>`).

**Correction (2026-07-12): 1237–1240 are NOT spare.** An earlier draft of this
ADR claimed they were free "future browser service" ports and that the
port-forward was already solved. They are in fact the **ADR 0029 docs pool** —
`site_serve::pool_ports() = 1237..=1240` (`POOL_SIZE=4`), pre-bound by the
daemon for per-connection root-relative site serving (confirmed live: `sotd`
holds 1235–1240). So **every forwarded port 1234–1240 is taken**, and WGLMakie
needs a **new** port: **1241** (first free above the daemon range). That means
the port-forward is *not* free — the launcher must be extended to forward 1241
(`-L 1241:127.0.0.1:1241`, added to `AUX_PORTS`), and a remote FE must
re-establish its tunnel to pick it up. This is the one real infra cost the
first draft missed.

## Decision

### 1. The Bonito server lives in the REPL, not a sidecar

Pluto runs as a **backend-supervised sidecar** (`backend/src/pluto.rs` →
`julia/pluto/start.jl`). WGLMakie **cannot** use that model: Bonito serves the
scene from the process that constructed the figure, and the user's figures are
built in the **REPL** process. A separate sidecar can't serve them. So the Bonito
server is embedded in the REPL, bound to `127.0.0.1:$SOT_WGL_PORT` (default
**1241**) with a loopback-shaped `proxy_url` so every URL it emits resolves
through the tunnel (once the launcher forwards 1241).

Interactivity survives the tunnel: Bonito serves HTTP **and** the WebSocket on the
one port (WS upgrade on the same connection), and SSH `-L` forwards the whole TCP
stream including the upgrade. Local (kitt-native) FE reaches `127.0.0.1:1241` directly; remote FE via `-L 1241:127.0.0.1:1241` (once the launcher forwards it). Same URL, both paths.

### 2. WGLMakie is never a dependency of `ShipToolsRepl`

The REPL child runs user code in the *user's* project (`--project=<user_project>`
+ `JULIA_LOAD_PATH=<repl_project>:`, see `backend/src/repl.rs`), so `using
WGLMakie` resolves from whatever WGLMakie the user's env pins. `ShipToolsRepl`
stays plotting-free and backend-agnostic; the serving helper resolves
`Main.Bonito`/`Main.WGLMakie` dynamically at call time and errors cleanly if they
aren't loaded. This keeps REPL startup/precompile light (Makie is heavy) and
avoids coupling the shim to one plotting stack.

### 3. A new `browser` REPL frame kind carries the URL — no new op, no backend change

`ReplFrame` is a `kind`-tagged enum built for additive frame kinds (ADR 0009).
We add `Browser { url }`. The backend relays `repl.frame` evts opaquely
(`ReplFrameMsg.frame` is passthrough JSON), so **the backend needs no change** —
only the two ends:

- **REPL**: returning a `BrowserView(url)` as an eval's last expression makes
  `value_frames_for` emit `{kind:"browser", url}` (checked before the image/text
  MIME probes, since a served figure may also be `showable` as an image). A
  bare `browserview(url)` is the exported convenience constructor.
- **FE**: `drain_events` intercepts a `Browser` frame *before* the repl-log
  append and hands the URL to `open_url_in_browser` (the pluto/video/docs path),
  setting a status line. It is an action, not log content, so it is never
  appended to the drawer (a defensive render arm exists only for exhaustiveness).

This makes "open this served URL in the FE browser" a **generic REPL primitive**.
WGLMakie/Bonito is the first producer; a served dashboard or any other
loopback-URL artifact rides the same rail.

### 4. Port config mirrors the siblings

`SOT_WGL_PORT` (default 1241), read REPL-side. The launcher **forwards it** —
added to `scripts/launch-sot.sh`, `scripts/launch-sot.ps1`, and `install.sh`'s
generated remote launcher alongside the pluto/video/docs/pool `-L` forwards
(both the aux-only and control-port tunnel blocks). An env override on both ends
keeps parity with `SOT_PLUTO_PORT` / `SOT_VIDEO_PORT` / `SOT_DOCS_PORT`. A remote
FE picks up the new forward by re-running its launcher.

## Status of the pieces

**Implemented (this branch):** `ReplFrame::Browser`; `BrowserView` /
`browserview` + the `value_frames_for` branch (+ REPL test); FE side-effect open
+ status (+ defensive render arm). Compiles; FE 190 + protocol 4 + REPL 36 tests
green.

**Proof env + validation (2026-07-12).** WGLMakie/Bonito are now materialized in
a dedicated env `dev/wglmakie-proof/` (**WGLMakie 0.13.13, Bonito 5.1.0, Makie
0.24.13, julia 1.12.5**); `dev/wglmakie-proof/run.jl` is the runnable proof (hit
`r`). The **Bonito 5.1.0 API is pinned** (headless-validated on kitt):

```julia
Bonito.configure_server!(; listen_url="127.0.0.1", listen_port=1241,
                           proxy_url="http://127.0.0.1:1241")   # NOT external_url
server = Bonito.Server(Bonito.App(fig), "127.0.0.1", 1241; proxy_url="http://127.0.0.1:1241")
url    = Bonito.online_url(server, "/")                          # http://127.0.0.1:1241/
```

Headless proof on kitt loopback: the server **binds 1241 and serves HTTP 200** — a
3.4 KB bootstrap page containing `Bonito` + `WGLMakie` + the ES `module` import
(the WebSocket is opened client-side by that JS from the page origin). So the API
and the HTTP layer are proven. **The one remaining unknown** is in-browser
interactivity (render + WS) *through the SSH forward* — which needs a real FE
browser and a `-L 1241` tunnel that does not exist yet. Once confirmed, `wglshow`
folds the three calls above into `ShipToolsRepl` (resolving `Main.Bonito`
dynamically) and returns `browserview(url)`.

## Consequences / known limits

- **`wglshow` deferred** until the proof pins the Bonito API. Until then, the
  primitive is usable by hand: serve via Bonito on 1241, then
  `return browserview(url)`.
- **Broadcast open**: `repl.frame` evts fan out to *all* attached FEs, so a
  `browser` frame currently opens the browser on every FE. Acceptable for the
  single-FE norm; a later refinement can gate the auto-open on the frame's
  `workspace_id` matching the FE's active workspace (as preview filtering does).
- **Focus**: opening a browser window is a deliberate, user-initiated action
  (the user returned a `BrowserView`), so it does not violate the no-yank
  show-image doctrine — it is the requested show, on the user's own machine.
- **Lifecycle**: the Bonito server lives as long as the REPL process; an `r`
  restart (project bounce) drops it and the next figure re-binds 1241.
