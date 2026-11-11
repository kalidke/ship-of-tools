# docs/: records (charter)

## Idea
`docs/history/adr` records why a decision was made, at the time; `docs/src` tells a user what is true now. A page changes in
the commit that changes the code it describes, and a design that is not built is marked unbuilt where it is described.

## Owns
- `docs/`: the ADRs and their index, the manual (`src/`, `make.jl`, `Project.toml`), `INSTALL-AGENT.md`,
  `UPDATE-AGENT.md`, `ENROLLING-A-HOST.md`, `USING.md`, `SESSION.md`, `SCREENSHOTS.md`, `plan.md`, the ownership table
  `ownership.md`, the list of known defects `defects.md` and the demo project the screenshots are taken from.
- Outside this folder: `requirements.md` and `README.md` at the root, and the root `CLAUDE.md`. Each folder's own
  `CLAUDE.md` belongs to the subsystem that owns that folder.

## Promises
- Line 3 of every ADR starts with one status token: `current`, `superseded by ADR NNNN` or `partly superseded by ADR
  NNNN`. `history/adr/README.md` lists the records under those tokens; the two checks in its "Maintaining this invariant"
  section are run by hand, and no CI job runs them.
- An ADR is changed by a dated amendment inside it or by a new ADR that supersedes it, and the status token says
  which.
- `make.jl` stages four single-source pages into `src/` at build time (`requirements.md`, `plan.md`, `comm/PROTOCOL.md`
  and `ENROLLING-A-HOST.md`); the staged copies are gitignored. Its built-site checks fail the build on a literal
  `<kbd>` and on a link to a missing section id, and doctests run inside it with both documented modules loaded.
- Published text names no private hostname, username or LAN detail, and the screenshots are generated from
  `fixtures/DemoProject`, never from a private workspace.
- `defects.md` is the one list of known defects and open items, grouped by the subsystems `package.md` names; each item
  has an UPPER-KEBAB id and a mark (seen by a person, seen by a session operating the product, a note from reading
  code, or a feature), and a landing that fixes one moves its item to Closed in the same commit. The local gate runs
  `scripts/tests/test-defects-check.sh` in every run, scoped or not: it fails with the id when a landing record
  (the section "Landing records since 2026-11-06" of `history/passes.md` to its end) names an item that is not in
  `defects.md`, when an id starts two items, when an open item has no mark, and when a tracked `CLAUDE.md` cites
  `defects.md` as `docs/defects.md: ID` or `docs/defects.md item ID` after an opening parenthesis with no known id. An id is a token of capitals
  and digits joined by hyphens with at least two parts that each hold two or more capitals, bounded by anything but a
  letter, digit, underscore or a hyphen that joins more: `ROW-ID-MINT` is one; `P1`, `P1-P4`, `ALLDONE`, `RCG_SCOPE` and
  `SHA-256` are not. The check does not prove that a fix moved its item to Closed.
  The list holds no private name and no unfixed security weakness. No session loads it at start and no `@` include
  names it; a pass reads its own subsystem's part.
- Nothing below `docs/` carries a page: `history/`, `fixtures/`, `tools/` and `src/` are described here, so `src/` holds only
  manual pages.

## Connections
Each connection is one row of docs/integration.md, owned by its provider. Provides: `docs/make.jl`,
`.github/workflows/CI.yml`. Uses: `sot-fe preview`, `docs/tools/docs-media.sh`,
`scripts/install.sh`, `docs/INSTALL-AGENT.md`.

## Folders
- `CLAUDE.md` (repo root): the design and conventions page every session loads.
- `docs/history/adr/`: the decision records.
- `docs/fixtures/`: `DemoProject`, the fixture project for docs media.
- `docs/tools/`: the docs media scripts: docs-media.sh, run by hand from SCREENSHOTS.md. Its cleanup trap follows
  comm/CLAUDE.md's rule; its rows are in agents/tests/test-temp-files.sh.
- `docs/src/`: the manual's pages and assets (covered by this page: the Julia environment in `Project.toml` makes
  `docs/` a package root).

## Files
- `ENROLLING-A-HOST.md`: the procedure for enrolling a host behind the hub; staged into the manual by `make.jl`.
- `INSTALL-AGENT.md`: the install runbook written for a coding agent.
- `UPDATE-AGENT.md`: the update runbook written for a coding agent, the one update procedure the others point at; its
  step 2 puts every daemon a window dials on the release's line before that window moves.
- `defects.md`: the list of known defects and open items; its head states the form of an item and the check.
- `Project.toml`: the docs environment (Documenter, DocumenterVitepress, and the two local packages by path).
- `SCREENSHOTS.md`: how the docs media are regenerated.
- `SESSION.md`: what every session but a subagent reads at its start; the root `CLAUDE.md` names it.
- `USING.md`: the entry page for a user with a local checkout.
- `history/`: the decision records (`history/adr/`, with their `README.md` index), the retired folder pages, the
  passes' measured history (`passes.md`) and the project chat's plan of record (`slack-client-plan.md`); no page loads
  them.
- `fixtures/`: `DemoProject`, the demo project the media are taken from.
- `integration.md`: how the subsystems connect: one table of every connection, then six walk-throughs with a file and a function at every hop.
- `make.jl`: builds the manual and runs the built-site checks.
- `ownership.md`: the one table of who owns each state file, process, op, setting and lock, and the pairs of copies that remain.
- `package.md`: the package page, loaded by the root CLAUDE.md: the subsystems, their guarantees and dependencies, the
  decisions in force, the standard for a landing and the current state; a landing's record goes to `history/passes.md`.
- `plan.md`: the phase-1 implementation plan; staged into the manual as the roadmap.
- `tools/`: the docs media scripts: docs-media.sh, run by hand from `SCREENSHOTS.md`.
- `src/.vitepress/`: the VitePress site configuration and theme.
- `src/assets/`: images, media and stylesheets the pages use.
- `src/components/`: the Vue components that show a demo still or loop.
- `src/concepts/`: concept pages (sessions, work state, the comm relay, transport).
- `src/contributing.md`: working conventions, the decision process, how to build and test.
- `src/design/`: design notes for planned or partly built parts (the staged roadmap and requirements pages land here).
- `src/extend/`: how to extend the system (the FileType ABI, the HDF5 tutorial and discovery notes).
- `src/features.md`: what the system does.
- `src/guide/`: task guides and architecture pages.
- `src/index.md`: the home page.
- `src/license.md`: the license page.
- `src/ref/`: reference pages (keybindings, configuration, API; the staged comm protocol page lands here).
- `src/start/`: quickstart, install and setup pages.

## Start here
`make.jl` for how the manual is assembled and what its checks reject; `history/adr/README.md` for the status tokens and the
checks that keep the index true.
