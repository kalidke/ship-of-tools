# ADR 0032 — Interactive browser-served figures (WGLMakie/Bonito)

**Status: PROPOSED — transport primitive IMPLEMENTED, WGLMakie serving PENDING PROOF**
(2026-07-11, branch `feat/wglmakie-browser`).

Landed on the branch: the `browser` REPL frame kind + `BrowserView`/`browserview`
in `ShipToolsRepl` + FE browser-open on that frame (Rust builds, 190 FE tests +
36 REPL tests green). Pending: the WGLMakie/Bonito serving convenience
(`wglshow`) and end-to-end validation of interactivity over the SSH tunnel —
gated on a real WGLMakie env (`dev/wglmakie-proof.jl`), which no kitt depot has
materialized yet.

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
(`install.sh`/`launch-sot.sh`: `-L <p>:127.0.0.1:<p>`). **1237–1240 are already
forwarded and unassigned** — provisioned as spare "future browser service" ports.
WGLMakie is that future service, so the port-forward is already solved.

## Decision

### 1. The Bonito server lives in the REPL, not a sidecar

Pluto runs as a **backend-supervised sidecar** (`backend/src/pluto.rs` →
`julia/pluto/start.jl`). WGLMakie **cannot** use that model: Bonito serves the
scene from the process that constructed the figure, and the user's figures are
built in the **REPL** process. A separate sidecar can't serve them. So the Bonito
server is embedded in the REPL, bound to `127.0.0.1:$SOT_WGL_PORT` (default
**1237**) with a loopback-shaped `proxy_url` so every URL it emits resolves
through the existing tunnel.

Interactivity survives the tunnel: Bonito serves HTTP **and** the WebSocket on the
one port (WS upgrade on the same connection), and SSH `-L` forwards the whole TCP
stream including the upgrade. Local (kitt-native) FE reaches `127.0.0.1:1237`
directly; remote FE via `-L 1237:127.0.0.1:1237`. Same URL, both paths.

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

`SOT_WGL_PORT` (default 1237), read REPL-side. The launcher already forwards it;
an env override on both ends keeps parity with `SOT_PLUTO_PORT` / `SOT_VIDEO_PORT`
/ `SOT_DOCS_PORT`.

## Status of the pieces

**Implemented (this branch):** `ReplFrame::Browser`; `BrowserView` /
`browserview` + the `value_frames_for` branch (+ REPL test); FE side-effect open
+ status (+ defensive render arm). Compiles; FE 190 + protocol 4 + REPL 36 tests
green.

**Pending proof (`dev/wglmakie-proof.jl`):** the exact current Bonito API for
binding a fixed loopback port with a matching external/proxy URL and obtaining a
figure's URL. WGLMakie/Bonito are **not materialized in kitt's default depot**;
several projects declare them (SMLMVis, SMLMView, papers-vortex-sr) but
instantiating one is a heavy download+precompile — a maintainer-triggered step,
not something a session should kick off. The proof validates two unknowns before
`wglshow` is folded into `ShipToolsRepl`: (a) that an interactive figure is fully
usable in the FE browser through the `-L 1237` tunnel (WS included), and (b) the
precise Bonito calls.

## Consequences / known limits

- **`wglshow` deferred** until the proof pins the Bonito API. Until then, the
  primitive is usable by hand: serve via Bonito on 1237, then
  `return browserview(url)`.
- **Broadcast open**: `repl.frame` evts fan out to *all* attached FEs, so a
  `browser` frame currently opens the browser on every FE. Acceptable for the
  single-FE norm; a later refinement can gate the auto-open on the frame's
  `workspace_id` matching the FE's active workspace (as preview filtering does).
- **Focus**: opening a browser window is a deliberate, user-initiated action
  (the user returned a `BrowserView`), so it does not violate the no-yank
  show-image doctrine — it is the requested show, on the user's own machine.
- **Lifecycle**: the Bonito server lives as long as the REPL process; an `r`
  restart (project bounce) drops it and the next figure re-binds 1237.
