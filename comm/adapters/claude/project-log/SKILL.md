---
name: project-log
description: Scaffold and maintain a hand-written HTML "project record" under project-log/ in a repository -- no build step, no Julia logic, plain files. Four verbs -- init (scaffold from the vendored skeleton), add-experiment (new experiment page, registered in the nav), add-journal (new dated journal entry), update-assets (refresh generic assets/templates only, never page content). Activates for "project log", "project record", "add experiment page", "add journal entry", "update project-log assets", "/project-log".
---

# project-log

Maintains a `project-log/` directory: hand-written HTML pages, a vendored KaTeX
copy for math, and one small script (`assets/site.js`) for navigation and
in-place page swaps. No build step — every page is served as-is. This skill
never writes prose into a page for you; it copies templates, fills the fields
that are mechanically determined by the command's own arguments, and wires the
new page into navigation. The conventions the pages themselves follow are
written out in full on `project-log/reference/how-we-work.html` once `init`
has run — read that page before writing content into a scaffolded page.

## Setup (every verb starts here)

Resolve the repo root and this skill's own `resources/` directory. Never
hardcode either path — the skill can be installed project-locally or in the
user's home, and the repo it's asked to act on is whatever the caller is in.

```bash
REPO="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

SKILL_DIR=""
for candidate in \
  "${CLAUDE_PROJECT_DIR:-$REPO}/.claude/skills/project-log" \
  "$HOME/.claude/skills/project-log"
do
  if [ -d "$candidate/resources" ]; then
    SKILL_DIR="$candidate"
    break
  fi
done
[ -n "$SKILL_DIR" ] || { echo "project-log skill resources/ not found next to SKILL.md" >&2; exit 1; }
RESOURCES="$SKILL_DIR/resources"
```

Two small helpers used below, for turning a CLI-style slug into a display
title and a human title into a slug:

```bash
# NOTE (skill-text rule): Claude Code substitutes positional-argument tokens
# (a dollar sign followed by a digit, and the all-arguments variable) into a
# skill's text when the skill is invoked with arguments, so NOTHING below may
# use a shell positional parameter or awk's whole-line variable — the helpers
# are stdin filters and awk uses the parenthesized form of that variable.
titleize() { # stdin "foo-bar_baz" -> "Foo Bar Baz"
  tr -- '-_' '  ' | awk '{for(i=1;i<=NF;i++)$i=toupper(substr($i,1,1)) substr($i,2); print}'
}
slugify() { # stdin "Foo Bar, baz!" -> "foo-bar-baz"
  tr '[:upper:]' '[:lower:]' | sed -E 's/[^a-z0-9]+/-/g; s/^-+|-+$//g'
}
```

---

## `init [--name "<Site name>"]`

Scaffolds `project-log/` into the current repo from this skill's vendored
skeleton.

1. **Refuse if `project-log/` already exists** — `init` never runs over an
   existing record.

   ```bash
   [ -d "$REPO/project-log" ] && { echo "$REPO/project-log already exists -- init refuses to overwrite it" >&2; exit 1; }
   ```

2. **Copy the whole skeleton.**

   ```bash
   cp -r "$RESOURCES" "$REPO/project-log"
   ```

3. **Set `SITE_NAME`.** Default is the repo directory name; `--name` overrides.

   ```bash
   NAME="<the value given after --name>"   # when no --name was given: NAME="$(basename "$REPO")"
   sed -i "s|^const SITE_NAME = \"project-name\";\$|const SITE_NAME = \"$NAME\";|" \
     "$REPO/project-log/assets/site.js"
   ```

4. **Leave `SITE_SECTIONS` as shipped.** It already lists Start, Charter,
   Computational experiments, Wet-lab experiments, Journal, and Reference,
   with the two empty experiment sections and no experiment pages — that's
   correct for a brand-new record (`site.js` hides a section with no pages).

5. **Print the file map and the two per-project values.**

   ```bash
   find "$REPO/project-log" -type f | sort
   grep '^const SITE_NAME = ' "$REPO/project-log/assets/site.js"
   awk '/^const SITE_SECTIONS = \[$/,/^\];$/' "$REPO/project-log/assets/site.js"
   ```

6. **Suggest a CLAUDE.md snippet — print it, never write it.** Offer this (or
   the like), adapted from `reference/how-we-work.html`, for the user to paste
   into their own `CLAUDE.md` if they want it:

   ```markdown
   ## Project record (`project-log/`)

   Hand-written HTML, no build step. Three tiers at three speeds: `charter/`
   is frozen (amendments only, dated), `status.html` is rewritten in place
   and describes only the present, `journal/` is append-only (newest first,
   never edited after the day it's written). Every experiment gets one page
   under `experiments/<campaign>/`, in this order: In short, Hypothesis,
   Methods, Results, Conclusions, Provenance. Add pages with the
   `/project-log` skill (`add-experiment`, `add-journal`); refresh generic
   assets/templates with `update-assets`. Full conventions:
   `project-log/reference/how-we-work.html`.
   ```

---

## `add-experiment <campaign> <page-slug> [--kind computational|wet-lab]`

Adds one experiment page and registers it in the navigation. `<campaign>` and
`<page-slug>` are slug-form (lowercase, hyphenated) — they become the folder
and file name directly. Default kind is `computational`.

```bash
CAMPAIGN="<campaign>"
SLUG="<page-slug>"
KIND="${KIND:-computational}"   # or wet-lab

case "$KIND" in
  computational) DATA_KIND="Computational experiment"; BADGE_KIND="Computational"; SECTION="Computational experiments" ;;
  wet-lab)       DATA_KIND="Wet-lab experiment";       BADGE_KIND="Wet lab";       SECTION="Wet-lab experiments" ;;
  *) echo "unknown --kind: $KIND (use computational or wet-lab)" >&2; exit 1 ;;
esac

TITLE="$(printf '%s' "$SLUG" | titleize)"
CAMPAIGN_TITLE="$(printf '%s' "$CAMPAIGN" | titleize)"
TODAY="$(date +%F)"

DEST="$REPO/project-log/experiments/$CAMPAIGN/$SLUG.html"
[ -f "$DEST" ] && { echo "$DEST already exists" >&2; exit 1; }
mkdir -p "$(dirname "$DEST")"
cp "$REPO/project-log/templates/experiment.html" "$DEST"
```

Fill the fields the template defines — title, kind, and the provenance date
(status stays the shipped default, `badge-planned` / "Planned", which is
correct for a page that was just created):

```bash
sed -i "s|<title>Experiment title · project record</title>|<title>$TITLE · project record</title>|" "$DEST"
sed -i "s|data-page=\"experiments/CAMPAIGN/SLUG.html\" data-kind=\"Computational experiment\"|data-page=\"experiments/$CAMPAIGN/$SLUG.html\" data-kind=\"$DATA_KIND\"|" "$DEST"
sed -i "s|<p class=\"eyebrow\">Campaign name · Experiment name</p>|<p class=\"eyebrow\">$CAMPAIGN_TITLE · $TITLE</p>|" "$DEST"
sed -i "s|<h1>Experiment title</h1>|<h1>$TITLE</h1>|" "$DEST"
sed -i "s|<span class=\"badge badge-kind\">Computational</span>|<span class=\"badge badge-kind\">$BADGE_KIND</span>|" "$DEST"
sed -i "s|<tr><th>Date</th><td>YYYY-MM-DD</td></tr>|<tr><th>Date</th><td>$TODAY</td></tr>|" "$DEST"
```

The Provenance date is the page's creation date, a stub — update it to the
actual run date once the experiment executes. Everything else in the
template's fixed sections (In short, Hypothesis, Methods, Results,
Conclusions, and the rest of Provenance) is placeholder prose left for
whoever writes the experiment.

Register the page in `SITE_SECTIONS`, under the matching kind section,
appended after any pages already there — this single script works whether
the section is currently empty or already has entries:

```bash
NEWLINE="      [\"experiments/$CAMPAIGN/$SLUG.html\", \"$TITLE\"],"
SITEJS="$REPO/project-log/assets/site.js"
TMP="$(mktemp)"
awk -v section="$SECTION" -v newline="$NEWLINE" '
  $(0) == "    title: \"" section "\"," { insection=1 }
  insection && $(0) == "    pages: [],"  { print "    pages: ["; print newline; print "    ],"; insection=0; next }
  insection && $(0) == "    pages: ["    { print; inpages=1; insection=0; next }
  inpages && $(0) == "    ],"            { print newline; print; inpages=0; next }
  { print }
' "$SITEJS" > "$TMP"
mv "$TMP" "$SITEJS"
```

Print the resulting file list and the `SITE_SECTIONS` diff:

```bash
find "$REPO/project-log/experiments" -type f | sort
awk '/^const SITE_SECTIONS = \[$/,/^\];$/' "$SITEJS"
```

---

## `add-journal [--date YYYY-MM-DD] "<title>"`

Adds one dated journal entry. Default date is today. Journal entries are
**not** added to `SITE_SECTIONS` — only `journal/index.html` links them (per
`reference/how-we-work.html`, the sidebar lists `journal/index.html` alone).

```bash
DATE="${DATE:-$(date +%F)}"
TITLE="<title>"
SLUG="$(printf '%s' "$TITLE" | slugify)"

DEST="$REPO/project-log/journal/$DATE-$SLUG.html"
[ -f "$DEST" ] && { echo "$DEST already exists" >&2; exit 1; }
cp "$REPO/project-log/templates/journal-entry.html" "$DEST"

sed -i "s|<title>YYYY-MM-DD · Entry title · project record</title>|<title>$DATE · $TITLE · project record</title>|" "$DEST"
sed -i "s|data-page=\"journal/YYYY-MM-DD-slug.html\"|data-page=\"journal/$DATE-$SLUG.html\"|" "$DEST"
sed -i "s|<p class=\"eyebrow\">Journal · YYYY-MM-DD</p>|<p class=\"eyebrow\">Journal · $DATE</p>|" "$DEST"
sed -i "s|<h1>Entry title</h1>|<h1>$TITLE</h1>|" "$DEST"
```

The command takes no author or campaign argument, so the `entry-meta` line
(`Written by NAME ... Campaign: NAME.`) is left as shipped — fill it in by
hand, or pass along the author/campaign if you know them when writing the
entry's content.

Add one row to `journal/index.html`, newest first — insert it right after the
`<tbody>` opening tag, ahead of whatever rows (or the authoring comment) are
already there:

```bash
IDX="$REPO/project-log/journal/index.html"
ROW="            <tr><td>$DATE</td><td><a href=\"$DATE-$SLUG.html\">$TITLE</a></td><td></td></tr>"
TMP="$(mktemp)"
awk -v row="$ROW" '{ print; if ($(0) == "          <tbody>") print row }' "$IDX" > "$TMP"
mv "$TMP" "$IDX"
```

Print the resulting file list:

```bash
find "$REPO/project-log/journal" -type f | sort
```

---

## `update-assets`

Refreshes `assets/` and `templates/` only, from this skill's vendored copy.
Never touches page content (`index.html`, `status.html`, `charter/`,
`experiments/`, `journal/`, `reference/`, `figures/`). Preserves the two
per-project values in `site.js` (`SITE_NAME`, `SITE_SECTIONS`) by capturing
them before the copy and re-applying them after.

```bash
SITEJS="$REPO/project-log/assets/site.js"
VERSION_BEFORE="$(cat "$REPO/project-log/assets/VERSION")"

NAME_SNIP="$(mktemp)"
SECTIONS_SNIP="$(mktemp)"
grep '^const SITE_NAME = ' "$SITEJS" > "$NAME_SNIP"
awk '/^const SITE_SECTIONS = \[$/,/^\];$/' "$SITEJS" > "$SECTIONS_SNIP"

cp -r "$RESOURCES/assets/." "$REPO/project-log/assets/"
cp -r "$RESOURCES/templates/." "$REPO/project-log/templates/"

TMP="$(mktemp)"
awk -v namefile="$NAME_SNIP" -v secfile="$SECTIONS_SNIP" '
  BEGIN { getline nameline < namefile }
  /^const SITE_NAME = / { print nameline; next }
  /^const SITE_SECTIONS = \[$/ {
    while ((getline secline < secfile) > 0) print secline
    inblock = 1; next
  }
  inblock && /^\];$/ { inblock = 0; next }
  inblock { next }
  { print }
' "$SITEJS" > "$TMP"
mv "$TMP" "$SITEJS"
rm -f "$NAME_SNIP" "$SECTIONS_SNIP"

VERSION_AFTER="$(cat "$REPO/project-log/assets/VERSION")"
echo "assets/VERSION before: $VERSION_BEFORE"
echo "assets/VERSION after:  $VERSION_AFTER"
```

---

## Invariants

- **`init` never runs over an existing `project-log/`.** It refuses and exits
  rather than merging or overwriting.
- **`update-assets` never overwrites content files.** It touches only
  `assets/` and `templates/` — `index.html`, `status.html`, `charter/`,
  `experiments/`, `journal/`, `reference/`, and `figures/` are untouched.
- **Nothing in `assets/` is hand-edited except `SITE_NAME` and `SITE_SECTIONS`
  at the top of `assets/site.js`.** Everything else in `assets/` (the rest of
  `site.js`, `site.css`, `katex/`, `VERSION`) is generic and only ever changes
  via `update-assets`. `update-assets` preserves `SITE_NAME`/`SITE_SECTIONS`
  by capturing their current values before the copy and re-applying them
  after — so an `update-assets` run never resets a record's name or nav list
  back to the shipped defaults, even though the file it overwrites is copied
  wholesale.
- **Templates are never hand-edited in place.** `templates/experiment.html`
  and `templates/journal-entry.html` are copied, then the copy is filled in —
  the template itself only changes via `update-assets`.
