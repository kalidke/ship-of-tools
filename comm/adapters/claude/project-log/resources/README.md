# project-log skeleton

A generic copy of the `project-log/` record template: hand-written HTML, no build step.
Copy this folder into a repository as `project-log/`, then fill in the per-project
values below. The conventions are on `reference/how-we-work.html`.

## Per-project values

| Where | What |
|---|---|
| `assets/site.js`, `SITE_NAME` | The name shown in the header (links to the front page). |
| `assets/site.js`, `SITE_SECTIONS` | The navigation list. Add a line when you add a page. Sections with no pages are not shown. |
| `index.html` | The front page. Replace every placeholder sentence. |
| `status.html` | The living state. Rewritten in place. |
| `charter/goals.html` | The first charter page. Add more charter pages beside it and list them in `SITE_SECTIONS`. |
| `reference/glossary.html` | The field terms assumed and the project terms defined. |

## Generic, refreshed by `update-assets`

`assets/site.css`, `assets/site.js` (everything below the per-project block),
`assets/katex/`, `assets/VERSION`, `templates/`. Refreshing them never touches page
content.

## Adding pages

- Experiment: copy `templates/experiment.html` to `experiments/<campaign>/<slug>.html`,
  set `data-page`, `data-kind` ("Computational experiment" or "Wet-lab experiment"),
  the kind badge, the status badge, and fill the fixed sections. Add it to
  `SITE_SECTIONS` under the matching kind.
- Journal entry: copy `templates/journal-entry.html` to
  `journal/YYYY-MM-DD-slug.html`, fill it, and add one row (newest first) to
  `journal/index.html`.
- Any page: the head block is identical across pages except the relative prefix
  (`assets/` at the root, `../assets/` one level down, `../../assets/` two levels
  down); `data-root` matches (`.`, `..`, `../..`); `data-page` is the path relative to
  the record root.

## Page anatomy

Eyebrow, `h1`, lead paragraph, an "In short" callout (plain words, no symbols, no
numbers), then sections with `id` attributes (the table of contents is built from
`h2`/`h3` with ids), then `<nav data-page-sequence>` for previous/next. Badges:
`badge-kind`, `badge-planned`, `badge-running`, `badge-complete`, `badge-qualified`,
`badge-superseded`. Callouts: `callout`, `callout result`, `callout warning`,
`callout success`. Tables sit inside `div.table-wrap`; figures inside
`figure.figure`; equations inside `div.equation` with `\[ ... \]`, inline with
`\( ... \)`.

## Behaviour provided by `assets/site.js`

Navigation from `SITE_SECTIONS`; in-place page swaps (fetch, swap the content column,
`pushState`; falls back to a normal navigation when fetch is unavailable, such as
`file://`); KaTeX auto-render; table of contents; previous/next; footer inside the
content column; sidebar scroll position kept in `sessionStorage`; an inline SVG
favicon. The sidebar is rendered once per load and never moves.
