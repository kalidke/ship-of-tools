# Docs screenshots

The images and loops under `docs/src/assets/media/` are generated from the
fixture, not from a maintainer's private workspace. Regenerate them whenever UI
changes make the published docs visually stale.

## Headless media pipeline

```bash
docs/tools/docs-media.sh list                     # the still, loop and feature tables
docs/tools/docs-media.sh stills [name...]         # PNG stills
docs/tools/docs-media.sh loops [name...]          # sessions
docs/tools/docs-media.sh features [name...]       # one short loop per feature
docs/tools/docs-media.sh all                      # stills, loops and features
docs/tools/docs-media.sh agent                    # the agent takes (real Claude Code)
docs/tools/docs-media.sh agent-cut                # cut the kept agent takes
```

What each still and loop shows, its key steps and its crop live in the
script, not here: the `STILLS` and `STILL_CROP` tables, `LOOPS` with the
`clip_*` functions and `build_loop`, the `FEATURES` table, and the agent
section (`clip_agent`, `agent_cut` and the crops above it). Read the
comments there.

Output goes to `docs/src/assets/media/`: a still is `<name>.png`; a loop is
`<name>.mp4` (H.264), `<name>.webm` (VP9) and `<name>.png` (poster).
`agent-cut` also writes `hero.gif` (the README fallback), `hero-phone.png`,
`agent-result.png`, `hero.speed.txt` and `layout-labelled.png` (through
`docs/src/assets/readme/make-layout.sh`, from the kept `layout.png`). The
README stills are made from media stills by `make-crops.sh`; re-run it after a
re-shoot of `state-colors.png`.

The script runs on a Linux box with no display and no GPU: `sot` and `sotd`
come from `SOT_MEDIA_BIN`, else the pinned build in `dev/output/docs-media-bin`,
else the installed build, else `rust/target/release`, and render through Mesa's
software Vulkan (lavapipe) into a private Xvfb, `ffmpeg` x11grab records, and a
small ctypes XTest driver presses the keys. It needs `Xvfb`, `ffmpeg` with
libx264 and libvpx, `python3`, `jq`, ImageMagick `convert`, and the lavapipe
ICD (`mesa-vulkan-drivers`). Shoot every published file on one build:
`docs/src/assets/media/BUILD.txt` names, for each published name, the
`sot --version` and `sotd --version` it was shot on (an agent file, the build
its take was recorded on), so a set that mixes builds shows it there.

- **Isolation.** Every process runs under `env -i` with a throwaway HOME, in a
  user, UTS and mount namespace whose hostname is `demo`, whose `/home` holds
  only `/home/demo` and the invoking user's home read-only, and whose
  `/etc/passwd` names the invoking uid `demo`. The project is a scratch copy of
  `docs/fixtures/DemoProject` at `/home/demo/DemoProject`. The scratch `sotd`,
  its five bash session rows, the frontend, Xvfb and ffmpeg are reaped by the
  EXIT trap, which also checks that no demo handle reached the real comm
  registry. `SOT_MEDIA_KEEP=1` keeps the scratch directory for debugging.
- **Takes.** Stills, loops and features are single live takes with no cuts;
  warm-up (kernel spawn, MathJax, PDF raster, HDF5 load, the first
  `scripts/route.jl` compile) happens before recording.
- **The nav pane's readouts.** The nav pane's `status:`, `annotation:` and
  `key:` rows are readouts with no switch to hide them, and the files that
  show the whole window show them as the window does: the hero,
  `layout-labelled`, and `nav-files`, `concept-stale`, `repl-figure`,
  `state-colors`, `sessions` (its `status:` row), `navigate-g`, `modules`
  and `pin`. The interface page says what each shows ("The lines around the
  tree"), and the home page links it under the hero.
- **No version label.** The window's bottom border (y 1002) carries a label
  naming the frontend and backend versions, and the session strip sits under
  it. No published file shows the label: the gallery crops end above it
  (`BODY_H`, y 990 at most; `preview-math`, right of the label, reaches the
  pane border), and the files that show the whole window (the hero, `repl`,
  `layout-labelled`) blank it and redraw the border under it (`VERSION_MASK`).

### Agent takes (`docs-media.sh agent`)

Every loop whose window shows the agent pane comes from one Claude Code
session in the DemoProject row's agent pane: the operator's own installed
Claude Code, in auto permission mode, costing a few requests on the account
the operator's own session runs on (its `CLAUDE_CONFIG_DIR`, else `~/.claude`). In order: the
hero asks for a bar chart of the legs in `scripts/route.jl`, run in the REPL
and shown in the preview, with a reply of a few lines, and ends when the
agent is idle; `navigate` browses data/ to the route figure while the
finished conversation stays in the pane, and `layout.png` is grabbed after
it with a run of the script in the drawer; then, each after `/clear`, `repl` asks the
agent to run the script in the REPL, `crop` sends a region of the zoomed
route figure with a question and `copy` pastes a copied path with a
question. `agent` records the takes into `dev/output/docs-media-agent/`
(lossless, with their times in `times.sh`, and `layout.png`); `agent-cut`
makes the published files from them, so crops and cuts change without a new
take.

- **Isolation.** `SOT_COMM_HOOKS=off` makes the product's Claude hooks no-op;
  `SOT_COMM_HOME`, `SOT_SOCKET` and `SOT_WORKSPACE_ID` point every comm, REPL
  and show-result call at the scratch daemon and the demo row. Only Claude
  Code's own config, cache and state paths are writable over the read-only
  home, and the exit trap removes the transcript it writes for
  `/home/demo/DemoProject`. The session reads no user or local settings
  (`--setting-sources project`), so the operator's hooks, `CLAUDE.md` and
  auto-memory stay out of it; the product's `julia-repl` and `show-result`
  skills are copied into the demo project's `.claude/skills`, and the flag
  settings allow `sot-fe`, `show-result`, and the Edit and Write tools in
  the demo project. They also turn off the skills bundled with Claude Code
  and those synced from the operator's claude.ai account and the spinner
  tips, and replace the rotating spinner verbs with one ("Working"); the
  demo settings also turn off Claude Code's suggested next prompt
  (`promptSuggestionEnabled` false).
  `--append-system-prompt` asks the agent to edit files with its edit tools
  and to call the pane a shown result opens in the preview.
  Read the takes before publishing all the same.
- **Layout.** The hero and navigate use columns `nav,preview,llm` (widths
  0.17, 0.27, 0.56), the whole window; repl and crop `preview,llm`; copy
  `nav,llm` with the drawer open, which scrolls the nav readouts out of the
  short nav pane. The agent pane is ~79 columns in all of them, so Claude
  Code never redraws for a column change; at ~54 columns its full-screen
  output overprinted itself as it scrolled, which is why the agent takes stay
  1920 wide. Its left border is at x 867 in the whole-window layout and at
  x 855 in the others (`NAV_PANE_X`, `AGENT_PANE_X`).
- **Banner.** Claude Code's welcome banner names the model and the account's
  plan, no setting hides it, and `/clear` does not remove it; under it sit
  a notice after Claude Code updated itself, a promo line and the `/clear`
  echo.
  `banner_masks` finds the `/clear` line in every frame of a take (a gray
  prompt mark, then text ending where "/clear" ends) and blanks the agent
  pane from its top row down through it in the pane's own color, so the
  logo, the plan, the promo line and `/clear` never show, in the hero or in
  the full-window `repl` loop alike; the gallery crops' agent rows start
  below it. Check a new cut's first seconds for any of them.
- **Cut.** The hero opens on the typed request, half a second after the
  prompt; the agent's work is sped up and the REPL run plays at real speed;
  `hero.speed.txt` records the ranges and factors for the caption, and the
  end frame (the agent idle after its summary) is the poster and
  `agent-result.png`. A `T_END` line in `times.sh` ends the hero earlier. A
  `REPL_SKIP=a:b` line cuts that span (raw seconds) out of the repl take
  (the published take needs none). In `repl`, `crop` and `copy` the lead-in
  up to 1 s before the question plays 3x, the wait for the answer 4x, and
  the answer's last frame is held 2 s (`T_ASK_<take>` in `times.sh`), so
  each loop spends most of its time on the result.
- **Crops.** `hero` is the whole window down through the session strip.
  `repl` is the window without its top row (the preview's title names the
  figure's scratch path under `.sot/runs`), down through the session strip;
  `repl-g`, `crop` and `copy` are the pane that carries the feature (drawer,
  zoomed figure, Files tree) and the agent's rows that hold the exchange, as
  two cards (the agent's rows start under the `/clear` line, y 384 in the
  published takes); `navigate-g` is the nav and preview panes; `hero.gif` is
  the whole hero at half width; `hero-phone.png` is the drawer's chart over
  the agent's reply; `navigate-g-phone.png` (DemoLoop's phone poster for
  `navigate-g`) is `navigate-g`'s end frame, the tree beside the route figure.

## One text scale

The frontend renders at font scale 1.5: a terminal cell is 27 px tall in
the 1920 px window. Every gallery file keeps the window's own pixels and is
a tight crop of the region it is about (`region_vf`), with no padding: the
site sizes each file from its own width (width x 0.75, `MEDIA_SCALE` in
`docs/src/components/media.ts`), so a cell is ~20.3 px on every page whatever
the crop's size, up to the widest doc column (1040 px): a file wider than
about 1390 px (`repl`, `layout-labelled`) shows at the column's width. A file that shows two
regions (`copy`, `crop`, `repl-g`, `sessions`) holds them as framed cards:
a 1 px `#3c3f44` border and 16 px of the page background `#1b1b1f` around
and between them, so a stack never reads as one broken pane, and nothing
beyond the cards.

| File | Region(s), window px | File size | Shown at | Cell |
|------|----------------------|-----------|----------|------|
| nav-files, repl-figure, modules, pin | nav + preview, 1260x974 | 1260x974 | 0.75 | 20.3 px |
| navigate-g, navigate-g-phone | nav + preview, 856x974 | 856x974 | 0.75 | 20.3 px |
| concept-stale | the maximized nav pane's rows | 1240x436 | 0.75 | 20.3 px |
| state-colors | the Sessions rows | 808x336 | 0.75 | 20.3 px |
| sessions | 808x336 over 794x46 | 842x434 | 0.75 | 20.3 px |
| repl | 1886x1022 | 1886x1022 | ~0.55 (the column) | 14.9 px |
| repl-g / crop / copy | 840x514 / 840x640 / 700x486 over 1024 wide | 1058 wide | 0.75 | 20.3 px |
| pdf | the preview's page area, plus its title row | 1256x974 | 0.75 | 20.3 px |
| help | 500x300 | same | 0.75 | 20.3 px |
| zoom | 664x476 | same | 0.75 | 20.3 px |
| preview-math | 664x990 | 664x990 | 0.75 | 20.3 px |
| preview-hdf5 | 664x590 | 664x590 | 0.75 | 20.3 px |
| hero, agent-result | 1920x1066 | 1920x1066 | ~0.60 | 16.2 px |
| hero.gif (README) | the hero at half width | 960x534 | ~0.43 | 11.7 px |
| hero-phone.png | 670x386 over 1016x166 | 1050x604 | ~0.34 | 9.3 px |
| layout-labelled.png | the window down through the session strip, a 76 px label band on top | 1920x1142 | ~0.54 (the column) | 14.6 px |

The hero is larger than the gallery on purpose (the landing page's one
loop, up to 1152 px wide): it is the whole window. The README GIF is the
whole hero at half width, the smallest text of the set, because the window
cannot be narrower than its ~79-column agent pane; the phone poster's agent
rows cannot wrap narrower than the pane either.

Check every new still and several frames of each loop for private content
before committing, for example
`ffmpeg -i hero.mp4 -vf fps=1,scale=480:-1,tile=4x4 -frames:v 1 tile.png`.


## Conventions

Use the committed fixture `docs/fixtures/DemoProject/` plus `examples/preview/`;
never capture a personal workspace. `.concept/files/src/DemoProject.jl.md` is
deliberately stale (an all-zero hash) for the drift badge; leave it alone.
`data/leg-notes.md` is the markdown-with-math sample; the PDF and HDF5 are
copied from `examples/preview/` at run time.

After replacing PNGs, run `oxipng -o 4 --strip safe docs/src/assets/media/*.png`
(the script does when `oxipng` is installed), then rebuild the docs and inspect
every page that embeds a changed file.
