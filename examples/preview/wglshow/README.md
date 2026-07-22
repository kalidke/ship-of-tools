# `wglshow` — interactive browser figures (ADR 0032)

`wglshow(fig)` serves a **WGLMakie** figure over Bonito on a loopback port and
the frontend auto-opens it in your OS browser: a live figure you can pan, zoom,
and rotate — not the static PNG that the preview pane shows for a saved plot.

## Run it

This directory **is its own project** — the [`Project.toml`](./Project.toml)
here declares `WGLMakie` as a **per-project dependency** (never global). Run the
demo with that project active, so `using WGLMakie` resolves against the
example's own env (the #44 per-package env-fix: a workspace REPL uses its
workspace's project). Either:

- point a Ship of Tools **workspace** at `examples/preview/wglshow/`, or
- in the REPL drawer: `] activate examples/preview/wglshow` then `] instantiate`
  (first time — resolves + precompiles WGLMakie).

Then, on [`wglshow_demo.jl`](./wglshow_demo.jl): `r` (fresh) / `R` (include) from
the nav, or paste it in. The **last expression must be `wglshow(fig)`** — its
return value (`BrowserView`) is what tells the frontend to open the browser.

First run precompiles WGLMakie (up to a minute); the REPL shows
*"julia starting — precompiling…"* until it's ready.

> **"Package WGLMakie not found in current path"** just means the active project
> doesn't declare WGLMakie — activate this example's project (above). The fix is
> a per-project dependency, **not** a global install.

## What it shows

A synthetic 3D localization cloud (two offset helical strands). Rotating it
reveals structure a flat projection hides — the reason to reach for `wglshow`
over a static render.

## Reach (local vs remote)

The browser opens `http://127.0.0.1:<port>/…` on both ends:

- **Local frontend** (same box as the daemon): reaches the loopback port directly.
- **Remote frontend**: the **daemon TCP proxy** (ADR 0035) carries the port
  through the one control tunnel — no per-port `ssh -L` needed. (The launcher's
  legacy `-L 1241` forward still works as a fallback and coexists.)

## Why WGLMakie, not CairoMakie

Ship of Tools' static previews use CairoMakie. `wglshow` is the interactive
path and needs WGLMakie's browser backend. `ShipToolsRepl` carries no plotting
dependency; `using WGLMakie` resolves it (and Bonito) from your project env at
call time. Pinned/validated against WGLMakie 0.13 / Bonito 5.1.
