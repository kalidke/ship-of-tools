---
name: show-result
description: Surface a result you just produced (plot, image, screenshot, PDF, HTML report, built doc, output file, published page) — badge it into the FE nav/preview pane, or open it in the OS browser. Never merely name a path or URL in text.
---

# show-result — put what you made in front of the user

The backend is headless (`DISPLAY` unset) — a browser/`xdg-open` there
reaches nobody. Fire this whenever your work produces something the user
should SEE (a plot/image/PDF/report/doc/site, or any moment your reply would
otherwise *name a path* or *paste a URL*), and on "open it in the browser",
"let me see it live" — those mean SHOW, not hand over a link.

## Three targets — pick by WHAT you made

| You produced | Command | Lands in |
|---|---|---|
| Image, figure, PDF, markdown, source file | `show-result <path>` | FE nav + preview pane (badge) |
| A built site, ANY page inside it, or a self-contained `.html` | `sot-fe docs <ws> <abs-path>` | FE machine's OS browser |
| An http(s) URL — page, PR, CI run, dashboard | `sot-fe open-url <url>` | FE machine's OS browser |

`open-url` is http(s)-only, no workspace. `docs` needs an ABSOLUTE backend
path (confined to a workspace root) — use it for anything that must actually
run (JS/WebGL). Both broadcast; `--fe <host>` targets the FE on that host.

**Point `docs` straight at the SUBPAGE you mean** — a project-log entry, one
page of a built manual, a report deep in a tree. The daemon finds the site
root for you (the outermost ancestor holding an `index.html`, never above the
workspace root) and opens your page relative to it, so stylesheets, sibling
assets and cross-page links all resolve. Do NOT open the site's index and
tell the user to click through, and do not paste a `file://` path: before
v0.6.6 a subpage was served from its own directory and arrived unstyled,
which is the failure this replaces.

## How

If the user asked for a specific file, badge it immediately — no
folder-reasoning — then `Read` it and give your critical read in words.

```bash
show-result <path>                                    # workspace-relative
show-result <path> --caption "Recovery vs. SNR, 3 densities, N=500"
```

Auto-discovers your workspace slug and calls `sot-fe preview`. Already
viewing your workspace → cursored + rendered immediately; otherwise the row
is badged and it renders the moment they switch — never a stolen session.

**One slot — the LAST show wins.** Every call replaces the pending
badge/preview, even one unseen — composite images into one figure first, or
pace a burst across turns. From the FE's Terminal drawer instead of a
backend session? Two env vars (`SOT_FE_ENDPOINT`, workspace slug) don't
auto-populate there — `references/from-frontend.md`.

**End your reply telling the user it's there**, e.g. *"...now showing in
your nav pane"* — only after `sot-fe` actually succeeded.

## `--caption` — say what the figure IS

Images only, drawn under the figure — write what you'd put under it in a
paper. Sticky to (workspace, file); a re-preview WITHOUT `--caption` clears
it. Max 300 chars.

## Don't

- Spam: one artifact per result — a burst of shows delivers only the last.
- Preview huge/unrenderable things (logs, build dirs, raw video) — use
  `reveal`/`notify`, or just name the path.
- Hand the user a link or call a headless backend a dead end — `open-url`/
  `docs` exist so the backend reaches the user's real browser.

**Bind it to the save** — run `show-result` on the same line you write the
file, so producing the output *is* surfacing it:

```julia
save(p, fig); run(`show-result $p --caption "$(describe(params))"`)
```
