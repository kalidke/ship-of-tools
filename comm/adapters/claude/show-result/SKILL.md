---
name: show-result
description: Surface a result you just produced (saved plot/figure, rendered image, screenshot, PDF, HTML report, built doc, output file, published page) where the user can actually see it — badge it into their Ship of Tools nav/preview pane, or open it in their real OS browser (`open-url` for an http(s) URL, `docs` for a local built site/HTML). Never merely name a path or a URL in text. Fire whenever your work creates something the user should SEE, including when they say "open it in the browser", "open the page", "launch it", or "let me see it live" — the backend is headless, so xdg-open on the backend reaches nobody.
---

# show-result — put what you made in front of the user

Ship of Tools exists to render results at native fidelity in the FE. A figure left on
disk and merely *named* in text defeats the whole premise — so does a URL merely
pasted. **When your work produces something the user should SEE, put it in front of
them — nav pane or real browser, whichever fits (see "Three targets") — before you
tell them it's done.** This is not optional polish — it is how a session delivers a
visual result.

## When — fire this whenever you produce a user-visible artifact

A saved plot/figure · a rendered image or screenshot · a PDF · an HTML report · a
built doc/site · a notebook export · a coverage/benchmark report · any output file the
user should look at · a published page or any URL you'd otherwise paste. **Concrete
trigger: if your final reply is about to *name a result path* or *paste a URL*, surface
it first.** Be aggressive — any saved plot, generated image, rendered doc, published
page, or notable output is a candidate.

**Fire on the words users actually say**, not just on your own artifact-production:
*"open it in the browser"* · *"open the page"* · *"launch it"* · *"let me see it live"*
· *"pull it up"*. Those are requests to SHOW, and they have a verb (see the next
section) — they are never a reason to hand the user a URL and ask them to open it
themselves.

## Three targets — pick by WHAT you made, not by what you have handy

These are easy to confuse, and confusing them is why a result goes unshown. **The
backend is headless** — `DISPLAY` is unset, and `xdg-open`/`chromium`/`firefox` run on
the backend reach *nobody*. "I can't open a browser from here" is never the answer;
it means you picked the wrong target.

| You produced | Verb | Lands in |
|---|---|---|
| An image, figure, PDF, markdown, source file | `show-result <path>` (→ `sot-fe preview`) | FE **nav + preview pane** (badge) |
| A **local** built site / self-contained `.html` on backend disk | `sot-fe docs <ws> <abs-path>` | FE machine's **OS browser** |
| An **http(s) URL** — published page, PR, CI run, dashboard, server you started | `sot-fe open-url <url>` | FE machine's **OS browser** |

```bash
~/.sot-comm/bin/sot-fe open-url https://example.org/some/page
~/.sot-comm/bin/sot-fe docs "$WS" /abs/path/to/site/index.html
```

- **`open-url` is http(s) only** and takes no workspace — the FE just opens it. Reach
  for it the moment your reply would otherwise *paste a link*: a PR you opened, a CI
  or release page, a published site, a dashboard.
- **`docs` needs an ABSOLUTE backend path** (it is stat'd raw, not resolved as a
  repo-relative node id) and is confined to a workspace root. Use it over `preview`
  for anything that must actually *run* — JS/WebGL, a multi-file site — since it
  serves the file to a real browser with no preview size caps. Not for http URLs.
- Both **broadcast** to every attached FE; `--fe <handle>` targets one.

## SHOW WHAT IS ASKED — immediately, unqualified. Then view it and annotate.

**Per the maintainer: the LLM does NOT qualify whether to show. If the user asked to
see a file, badge THAT file, immediately — no content judgment, no
folder-semantics reasoning, no "is this the right one" deliberation between
the ask and the show.** Your judgment happens AFTER the badge, as words:

- **Show first.** The exact path the user pointed at, unmodified.
- **Then `Read`-view what you showed** and give the user your read of it —
  for an analysis result, a one-line critical eval belongs WITH the figure
  ("full-field render, reads near-black at this scale; `panel_mapn.png` is
  the legible crop — badged as well").
- Additional context is ADDITIVE: badge companion artifacts too if they help;
  never un-show, never silently swap, never offer to regenerate something
  that already exists.

Why this rule exists (2026-07-03 incident): a session spent ~10 min and four
user corrections NOT showing a figure that existed — filename-guessing,
folder-reasoning, and offering regeneration instead of just showing the file
and looking at it. Domain note: full-field scientific renders are inherently
near-black at overview scale (nm features in a µm field); `show-result`
prints a near-blank warning on low pixel variance — that means "view it and
annotate", NEVER "don't show".

## How — one command; the show is complete when the user looks

One short command, right after you save the artifact:

```bash
show-result <path>            # workspace-relative (an absolute path under the ws auto-relativizes)
show-result <path> --caption "Recovery vs. SNR, 3 densities — 20 nm/px, N=500"
```

`show-result` (`~/.local/bin/show-result`, on PATH in a booted session) auto-discovers
your workspace slug and calls `sot-fe preview` for you. If it isn't on PATH, the
long form is:

```bash
WS=${SOT_WORKSPACE:-$(tmux display-message -p '#S' | sed -n 's/^sot-be-//p')}
~/.sot-comm/bin/sot-fe preview "$WS" "<path>"
```

**What happens — complete show, never a stolen session (maintainer semantics,
2026-07-10):** if the user is already viewing your workspace, the file is
cursored in the nav and rendered in the preview immediately. Otherwise the
workspace row is BADGED — the user's view is never yanked mid-work — and the
moment they switch to your workspace, the file is cursored in the nav AND
rendered in the preview, automatically and completely. "Show" means the nav
selection and the preview are BOTH set when seen; it does not mean capturing
the user's session. `--urgent --fe <handle>` is the explicit focus-capture
variant for when the user ASKS to see something now (a broadcast `--urgent`
is stripped FE-side — it cannot yank every screen). Tell the user in your
reply where the figure is badged.

**Discover your slug — never guess it.** Prefer `$SOT_WORKSPACE` (stamped when the
backend creates the workspace). If unset (an *attached* or re-shelled pane), strip
`sot-be-` from your tmux session name — the one-liner above does both.

### From a FRONTEND session (the FE's Terminal drawer) — two things differ

Everything above assumes a booted **backend** session. A Claude running in the
Windows FE Terminal drawer (`win-fe-<host>`, bootstrapped by
`/sot-fe-session-start`) has neither of the things the auto-discovery relies on,
and fails in a way that reads like a dead daemon:

```
ERROR: could not find the sotd daemon. Set --endpoint unix:/path or tcp:HOST:PORT (or $SOT_FE_ENDPOINT).
```

That message appears **even with the tunnel plainly working**. Two similarly-named
variables carry the same value to different tools, and only one of them is set for
you:

| Variable | Read by | Set by `/sot-fe-session-start`? |
|---|---|---|
| `SOT_RELAY_ENDPOINT` | `comm-relay.sh` (messaging) | **yes** |
| `SOT_FE_ENDPOINT` | `sot-fe` (`preview` / `docs` / `open-url`) | **no** — export it yourself |

The two fixes:

1. **`sot-fe` reads `$SOT_FE_ENDPOINT`, *not* `$SOT_RELAY_ENDPOINT`.**
   `/sot-fe-session-start` exports only the latter, so a bootstrapped FE session
   has a working tunnel that `sot-fe` cannot see. Export both — same value, the
   FE's local forwarded port:

   ```bash
   export SOT_FE_ENDPOINT="tcp:127.0.0.1:${SOT_PORT:-18743}"
   ```

2. **Pass the workspace slug explicitly — it cannot be derived here.** There is no
   tmux on the FE box and `$SOT_WORKSPACE` is unset, so the `WS=` one-liner above
   yields an empty slug and shows nothing. Name the target workspace:

   ```bash
   ~/.sot-comm/bin/sot-fe preview sot examples/preview/quarto_julia.qmd
   ```

The path stays **workspace-relative and backend-side** — it is resolved against the
target workspace on the daemon's disk, not against the FE machine's checkout, even
though you are typing it on the FE. Same two rules apply to `docs` and `open-url`.

## `--caption` — say what the figure IS

**Images only.** A caption is drawn in a band UNDER the figure (the image shrinks to
make room — it never covers the plot). Use it whenever the filename alone doesn't
say what the user is looking at, which is most of the time:

```bash
show-result out/recovery.png --caption "Recovery vs. SNR, 3 densities — 20 nm/px, N=500 emitters"
```

- **Write the caption you'd put under this figure in a paper** — what it shows and
  the parameters needed to read it. Not the filename restated, not "here is the plot".
- **It's sticky to that (workspace, file)**: it survives workspace switches and
  re-previews, so a badge the user reaches ten minutes later still carries it. That
  is the point — the caption is what makes a badge self-explanatory on arrival.
- **Re-badging the same file WITHOUT `--caption` clears it.** So when you regenerate
  a figure with different parameters, pass the new caption — a stale caption
  describing the previous run is worse than none.
- Max 300 chars — the CLI warns, the frontend does the cutting (on a character
  boundary, so multi-byte text survives). At a typical pane width that's about
  three lines, which is the band's limit too: write a caption, not a report.
- Ignored for non-image previews (code, markdown, PDF) — the store keeps it, the
  renderer declines to draw it, so nothing breaks if you pass one.

## After — tell them, but only if it worked

**End your reply telling the user it's there**, in those words — e.g. *"…and that
figure is badged on the papers-geometry row (it opens cursored + rendered when you
switch over)"*, or *"…now showing in your nav pane"* when they were already viewing
your workspace. Say so **only after `sot-fe` succeeded** — don't claim it if the
command errored.

## Don't

- **Don't spam.** Preview the ONE best artifact per result/milestone — for many outputs,
  the entry point (`index.html`, the final PDF, the main figure) — not every file/frame.
- **Don't preview huge or unrenderable things.** Massive logs/CSVs, build dirs, raw
  video (video shows a *poster only* — ADR 0018). Use `reveal` (cursor, no body) or
  `notify`, or just name the path.
- **Don't race the file.** Surface only after the artifact is fully written.
- **Don't guess the workspace.** Derive it (above); a wrong slug shows nothing.
- **Don't hand the user a link and ask them to open it**, and don't report a headless
  backend as a dead end — `open-url`/`docs` exist precisely so the backend can reach
  the user's real browser. Checking `DISPLAY` on the backend answers the wrong
  question.

## Even better — bind it to production

The most reliable surfacing doesn't depend on remembering this skill — **bind it to the
save.** Run `show-result` on the same line you write the file, so producing the output
*is* surfacing it:

```julia
save(p, fig); run(`show-result $p`)          # Julia / CairoMakie
save(p, fig); run(`show-result $p --caption "$(describe(params))"`)   # with a caption
```
```bash
mything --out out.png && show-result out.png  # shell pipeline
```

No remembering required: if the artifact got written, the badge fired.
