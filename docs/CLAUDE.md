# docs/: records (charter)

## Idea
`docs/adr` records why a decision was made, at the time; `docs/src` tells a user what is true now. A page changes in
the commit that changes the code it describes, and a design that is not built is marked unbuilt where it is described.

## Owns
- `docs/`: the ADRs and their index, the manual (`src/`, `make.jl`, `Project.toml`), `INSTALL-AGENT.md`,
  `ENROLLING-A-HOST.md`, `USING.md`, `SCREENSHOTS.md`, `plan.md` and the demo project the screenshots are taken from.
- Outside this folder: `requirements.md` and `README.md` at the root, the root `CLAUDE.md`, the media script
  `scripts/docs-shots.sh`, and the publish guard (`.claude/settings.json`,
  `.claude/hooks/`). Each folder's own `CLAUDE.md` belongs to the subsystem that owns that folder.

## Promises
- Line 3 of every ADR starts with one status token: `current`, `superseded by ADR NNNN` or `partly superseded by ADR
  NNNN`. `adr/README.md` lists the records under those tokens; the two checks in its "Maintaining this invariant"
  section are run by hand, and no CI job runs them.
- An ADR is changed by a dated amendment inside it or by a new ADR that supersedes it, and the status token says
  which.
- `make.jl` stages four single-source pages into `src/` at build time (`requirements.md`, `plan.md`, `comm/PROTOCOL.md`
  and `ENROLLING-A-HOST.md`); the staged copies are gitignored. Its built-site checks fail the build on a literal
  `<kbd>` and on a link to a missing section id, and doctests run inside it with both documented modules loaded.
- Published text names no private hostname, username or LAN detail; the publish guard scans commit, PR and issue
  text for them, and the screenshots are generated from `fixtures/DemoProject`, never from a private workspace.
- Nothing below `docs/` carries a page: `adr/`, `fixtures/`, `tools/` and `src/` are described here, so `src/` holds only
  manual pages.

## Connections
- `.github/workflows/CI.yml` (job `docs`) builds the manual with `make.jl` and deploys it on a push to `main`.
- The root `CLAUDE.md` sends a reader to ADR 0017 (frontend restart) and ADR 0023 and ADR 0046 (session spawn and daemon
  boot); code comments and folder pages cite ADRs by number.
- `docs/tools/docs-media.sh` and `scripts/docs-shots.sh` render the frontend against `fixtures/DemoProject`;
  `SCREENSHOTS.md` is their recipe.
- `INSTALL-AGENT.md` is the runbook an agent follows to install; its engine is `scripts/install.sh` (distribution
  charter: scripts/CLAUDE.md).

## Folders
- `.claude/hooks/`: the publish guard, records' other home with the root `CLAUDE.md`.
- `CLAUDE.md` (repo root): the design and conventions page every session loads.
- `docs/adr/`: the decision records.
- `docs/fixtures/`: `DemoProject`, the fixture project for docs media.
- `docs/tools/`: the docs media scripts: docs-media.sh, run by hand from SCREENSHOTS.md.
- `docs/src/`: the manual's pages and assets (covered by this page: the Julia environment in `Project.toml` makes
  `docs/` a package root).

## Files
- `ENROLLING-A-HOST.md`: the procedure for enrolling a host behind the hub; staged into the manual by `make.jl`.
- `INSTALL-AGENT.md`: the install runbook written for a coding agent.
- `Project.toml`: the docs environment (Documenter, DocumenterVitepress, and the two local packages by path).
- `SCREENSHOTS.md`: how the docs media are regenerated.
- `USING.md`: the entry page for a user with a local checkout.
- `adr/`: the decision records and their `README.md` index.
- `fixtures/`: `DemoProject`, the demo project the media are taken from.
- `make.jl`: builds the manual and runs the built-site checks.
- `plan.md`: the phase-1 implementation plan; staged into the manual as the roadmap.
- `tools/`: the docs media scripts: docs-media.sh, run by hand from `SCREENSHOTS.md`.
- `src/.vitepress/`: the VitePress site configuration and theme.
- `src/assets/`: images, media and stylesheets the pages use.
- `src/components/`: the Vue components that show a demo still or loop.
- `src/concepts/`: concept pages (sessions, work state, the comm relay, transport).
- `src/contributing.md`: working conventions, the decision process, how to build and test.
- `src/design/`: design notes for planned or partly built parts (the staged roadmap and requirements pages land here).
- `src/extend/`: how to extend the system (the FileType ABI, the HDF5 tutorial, discovery and mode notes).
- `src/features.md`: what the system does.
- `src/guide/`: task guides and architecture pages.
- `src/index.md`: the home page.
- `src/license.md`: the license page.
- `src/ref/`: reference pages (keybindings, configuration, API; the staged comm protocol page lands here).
- `src/start/`: quickstart, install and setup pages.

## Start here
`make.jl` for how the manual is assembled and what its checks reject; `adr/README.md` for the status tokens and the
checks that keep the index true.
