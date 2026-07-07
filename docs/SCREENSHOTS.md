# Docs screenshots — how they are made

The images under `docs/src/assets/screenshots/` come in two tiers. Anything
currently showing a dark "screenshot pending" card is a placeholder awaiting
its first real capture. **Placeholders must all be replaced before any
release or before the repo goes public.** (Interim: the maintainer OK'd merging to
main with the remaining Tier-2 placeholders on 2026-07-02 — the deployed
site is collaborator-only while the repo is private; the FE/kernel fixes on
the branch shouldn't wait on screenshots.)

All shots are staged against the committed fixture workspace
`docs/fixtures/DemoProject/` (plus `examples/preview/` for the file-preview
shots) — never against a real working project. Captures need a box that can
open a wgpu window (Windows FE box, or a Linux machine with a desktop; the dev
Linux box is headless — no X — and xvfb won't render wgpu/vulkan, so don't try).

## Tier 1 — scripted (regenerate any time)

```bash
scripts/docs-shots.sh run          # everything
scripts/docs-shots.sh run nav-files preview-math   # a subset
scripts/docs-shots.sh list         # print the exact per-shot invocations
```

`run` builds the binaries if needed, stamps the fixture's fresh `.concept`
annotation (`sync-fixture`), boots a scratch `sotd` per project root, and
drives the FE's `--capture` harness. On a box without bash (Windows), use
`list` output: each shot is one `sotd` line + one `sot` line, trivially
translated to PowerShell.

Capture-host dependencies beyond the normal dev stack (found by the first
Windows capture run):

- **MathJax sidecar**: `npm ci` in `rust/backend/sidecars/mathjax` — without
  `node_modules`, math previews show raw LaTeX. `run` does this automatically.
- **poppler** (`pdftoppm` + `pdfinfo`) on PATH for the PDF preview shot. On
  Windows a throwaway conda env works: `conda create -n sot-poppler poppler`.
- **Warm the stack before keeping shots**: the kernel is lazy-spawned, the
  MathJax sidecar boots async, and the *first* HDF5 render crashes-then-
  respawns the kernel. `run` fires a discarded warm-up capture per daemon;
  manual runs should do the same (or expect the first shot of each group to
  be cold). Keep per-shot delays ≥ 12 s.
- Long capture batches have hard-crashed an FE once on an AMD 780M
  (exit 0xCFFFFFFF after ~25 capture processes) — run in batches and let the
  supervisor respawn; captures are idempotent.

Conventions: **borderless fullscreen on an ultrawide monitor** (the matrix
passes `--start-fullscreen`; Ship of Tools is designed around ultrawide, so
published shots must show that layout — do not capture on a 16:9 panel),
`--contrast-mode bright`, dark theme, no personal workspaces connected. If you
change the fixture source, re-run
`sync-fixture` (and leave `route_length.md`'s zero hash alone — it is
*deliberately* stale for the drift-badge shot).

## Tier 2 — hand-staged live grabs (per the maintainer's hero spec, 2026-07-03)

Stage the LIVE dev environment — **fullscreen on the ultrawide**, the user's
zoom — and grab the window (on Windows: the `/selfie` DWM grab; crop to the
window rect). One rich hero session feeds several assets as crops.

**The hero session** (stage once, shoot several):

- **Multiple real workspaces** in the Sessions strip / state-nav, including
  at least one **worktree** row (the `.SoT` / `.SoT-wt-*` family grouping),
  with a **mix of work-state colors** (working / idle / waiting / blocked) —
  nudge comm sessions with `comm-status.sh` if the live mix is too uniform.
  Use public or synthetic examples only.
- **Nav in Files mode on the ship-of-tools repo, cursored on `logo.png`** —
  the preview pane shows the full Ship of Tools logo. No concept-mode
  column (not implemented yet — leave it out).
- **Orchestrator pane**: a short real exchange (e.g. ask the workspace agent
  to summarize `route_length`) that fits without scrolling.
- **REPL drawer open** with the fixture staging (below) — the `plot_route`
  figure inline, height-fit.

The fixture REPL staging (fixture env has CairoMakie as a dep — instantiate
first; `demo/stage_repl.jl` does the same in one include):

```julia
using DemoProject
route = [Waypoint("Albuquerque", 35.08, -106.65),
         Waypoint("Santa Fe",    35.69, -105.94),
         Waypoint("Black Mesa",  35.87, -106.08),
         Waypoint("Taos",        36.41, -105.57)]
route_length(route)     # scalar value frame (≈ 192.7 km)
plot_route(route)       # inline figure: labeled waypoints + per-leg distances
```

**Assets from the hero session:**

- **hero.png** — the full window as staged above, drawer = REPL (julia).
- **hero-monitor.png** — same session, `Ctrl+M` (drawer = Monitor).
- **hero-closed.png** — same session, drawer closed (the pure pane geometry
  for the panes-overview page).
- Further crops (session strip, nav column) may replace Tier-1 shots later —
  capture the hero at full quality and keep the uncropped original.

**Standalone Tier-2 shots:**

- **beauty.png** — "a nice full screen of something nice": Files mode
  cursored on `docs/fixtures/DemoProject/data/synthetic_map.png` (a synthetic
  super-resolution demo map), preview pane
  **maximized** (`Alt+=`) so the image nearly fills the window.
(The Terminal drawer is documented in prose only — no screenshot — on the
[Terminal pane](../src/guide/panes/terminal.md) page.)

A note on flavor: the staged content carries a few deliberate cultural nods —
the Black Mesa waypoint (a New Mexico research facility, after all), the
"sites of grace" framing in the fixture README, and the white-rabbit session
in the demo session strip. Keep them when re-staging; they are part of the
docs' voice. Don't add more per shot — one each is a wink, three is a bit much.

After replacing PNGs: `oxipng -o 4 --strip safe docs/src/assets/screenshots/*.png`
(or let `docs-shots.sh run` do it), rebuild the docs locally, eyeball every
page that embeds a shot, commit.
