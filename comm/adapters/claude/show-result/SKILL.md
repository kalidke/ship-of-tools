---
name: show-result
description: Surface a result you just produced (saved plot/figure, rendered image, screenshot, PDF, HTML report, built doc, output file) in the user's Ship of Tools nav/preview pane — BADGE it by default, never merely name it in text. Fire whenever your work creates something the user should SEE.
---

# show-result — put what you made in front of the user

Ship of Tools exists to render results at native fidelity in the FE. A figure left on
disk and merely *named* in text defeats the whole premise. **When your work produces
something the user should SEE, show it in their nav pane before you tell them it's
done.** This is not optional polish — it is how a session delivers a visual result.

## When — fire this whenever you produce a user-visible artifact

A saved plot/figure · a rendered image or screenshot · a PDF · an HTML report · a
built doc/site · a notebook export · a coverage/benchmark report · any output file the
user should look at. **Concrete trigger: if your final reply is about to *name a result
path*, surface that path first.** Be aggressive — any saved plot, generated image,
rendered doc, or notable output is a candidate.

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

## How — one command; the FE ALWAYS navigates to it and shows it

One short command, right after you save the artifact:

```bash
show-result <path>            # workspace-relative (an absolute path under the ws auto-relativizes)
```

`show-result` (`~/.local/bin/show-result`, on PATH in a booted session) auto-discovers
your workspace slug and calls `sot-fe preview` for you. If it isn't on PATH, the
long form is:

```bash
WS=${SOT_WORKSPACE:-$(tmux display-message -p '#S' | sed -n 's/^sot-be-//p')}
~/.sot-comm/bin/sot-fe preview "$WS" "<path>"
```

**What happens — deterministic, no variants (maintainer directive 2026-07-10):**
the FE switches to your workspace, sets the nav cursor on the file, and renders it
in the preview pane. Every time. There is no badge-only fallback, no "shows on
their next switch", no urgency tier — showing a result MOVES the user's view to
it, by design. (The old badge-floor/`--urgent` split let shown figures degrade to
unnoticed badges and read as broken; it is gone. `--urgent`/`--fe` are still
accepted for wire compat but change nothing. FEs on builds older than 2026-07-10
still badge — if the user reports nothing appeared, have them pull + rebuild.)

**Discover your slug — never guess it.** Prefer `$SOT_WORKSPACE` (stamped when the
backend creates the workspace). If unset (an *attached* or re-shelled pane), strip
`sot-be-` from your tmux session name — the one-liner above does both.

## After — tell them, but only if it worked

**End your reply telling the user it's there**, in those words, e.g. *"…and that figure
is now showing in your nav pane."* Say so **only after `sot-fe` succeeded** — don't
claim it if the command errored.

## Don't

- **Don't spam.** Preview the ONE best artifact per result/milestone — for many outputs,
  the entry point (`index.html`, the final PDF, the main figure) — not every file/frame.
- **Don't preview huge or unrenderable things.** Massive logs/CSVs, build dirs, raw
  video (video shows a *poster only* — ADR 0018). Use `reveal` (cursor, no body) or
  `notify`, or just name the path.
- **Don't race the file.** Surface only after the artifact is fully written.
- **Don't guess the workspace.** Derive it (above); a wrong slug shows nothing.

## Even better — bind it to production

The most reliable surfacing doesn't depend on remembering this skill — **bind it to the
save.** Run `show-result` on the same line you write the file, so producing the output
*is* surfacing it:

```julia
save(p, fig); run(`show-result $p`)          # Julia / CairoMakie
```
```bash
mything --out out.png && show-result out.png  # shell pipeline
```

No remembering required: if the artifact got written, the badge fired.
