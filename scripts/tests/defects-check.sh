#!/usr/bin/env bash
# defects-check.sh [--root <checkout>]: every item id a landing record names is an item of docs/defects.md.
# Landing records are the section "Landing records since 2026-11-06" of docs/history/passes.md to the end of the file.
# An id is a token of capitals and digits joined by hyphens, with at least two parts that each hold two or more
# capitals, bounded by anything but a letter, digit, underscore or a hyphen that joins more (ROW-ID-MINT is one; P1,
# P1-P4, ALLDONE, RCG_SCOPE and SHA-256 are not).
# An id is known when an item line `- **ID** ...` starts with it. Also refused: an id that starts two items, and an open
# item (outside the Closed section) with no mark [person], [session], [note] or [feature]. Also refused: a tracked
# CLAUDE.md that cites the list as `(docs/defects.md: ID` or `(docs/defects.md item ID` (more ids joined by "and" or a
# comma) with an id that is no item, or with no id at all. Prints one line per fault,
# naming the id; exit 1 on any fault, 2 on bad arguments or a missing file or heading. docs/CLAUDE.md owns the rule.
exec python3 -I - "$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)" "$@" <<'PY'
import os, re, sys

root, args = sys.argv[1], sys.argv[2:]
if args[:1] == ["--root"] and len(args) == 2:
    root = os.path.abspath(args[1])
elif args:
    print("usage: defects-check.sh [--root <checkout>]", file=sys.stderr)
    sys.exit(2)

LIST = os.path.join(root, "docs", "defects.md")
PASSES = os.path.join(root, "docs", "history", "passes.md")
HEADING = "## Landing records since 2026-11-06"
try:
    defects = open(LIST, encoding="utf-8").read()
    passes = open(PASSES, encoding="utf-8").read()
except OSError as e:
    print("defects-check: %s" % e, file=sys.stderr)
    sys.exit(2)

TOKEN = re.compile(r"(?<![A-Za-z0-9_-])[A-Z][A-Z0-9]*(?:-[A-Z0-9]+)+(?![A-Za-z0-9_]|-[A-Za-z0-9])")
ITEM = re.compile(r"^- \*\*([A-Z][A-Z0-9]*(?:-[A-Z0-9]+)+)\*\*(.*)$")
MARK = re.compile(r"^ \[(person|session|note|feature)\]")

def is_id(tok):
    return sum(1 for part in tok.split("-") if len(re.findall("[A-Z]", part)) >= 2) >= 2

faults = []
heads = {}
closed = False
for line in defects.splitlines():
    if line.startswith("## "):
        closed = line.strip() == "## Closed"
    m = ITEM.match(line)
    if not m:
        continue
    ident, rest = m.group(1), m.group(2)
    if ident in heads:
        faults.append("%s: starts two items of docs/defects.md" % ident)
    heads[ident] = True
    if not is_id(ident):
        faults.append("%s: an item head that is no id by the rule (two parts of two or more capitals)" % ident)
    if not closed and not MARK.match(rest):
        faults.append("%s: an open item with no mark [person|session|note|feature]" % ident)

at = passes.find("\n" + HEADING)
if at < 0 and not passes.startswith(HEADING):
    print("defects-check: %s has no section %r" % (PASSES, HEADING), file=sys.stderr)
    sys.exit(2)
records = passes[at + 1:] if at >= 0 else passes
seen = []
for tok in TOKEN.findall(records):
    if is_id(tok) and tok not in heads and tok not in seen:
        seen.append(tok)
        faults.append("%s: named in a landing record of docs/history/passes.md, not an item of docs/defects.md" % tok)

import subprocess
try:
    p = subprocess.run(["git", "-C", root, "ls-files", "-z", "--", "*CLAUDE.md"], stdout=subprocess.PIPE,
                       stderr=subprocess.DEVNULL, check=True)
    contracts = [f for f in p.stdout.decode("utf-8").split("\0") if f]
except (OSError, subprocess.CalledProcessError):
    contracts = []
    for dirpath, dirs, files in os.walk(root):
        dirs[:] = [d for d in dirs if d not in (".git", "target", "node_modules")]
        contracts += [os.path.relpath(os.path.join(dirpath, f), root) for f in files if f.endswith("CLAUDE.md")]
CITE = re.compile(r"\(docs/defects\.md(?::\s*|\s+item\s+)?")
IDS = re.compile(r"(%s)((?:(?:,|\s+and)\s+%s)*)" % (TOKEN.pattern, TOKEN.pattern))
for rel in sorted(contracts):
    try:
        text = open(os.path.join(root, rel), encoding="utf-8").read()
    except OSError:
        continue
    for m in CITE.finditer(text):
        got = IDS.match(text, m.end())
        if not got:
            faults.append("%s: cites docs/defects.md with no item id" % rel)
            continue
        for tok in TOKEN.findall(got.group(0)):
            if tok not in heads:
                faults.append("%s: cites %s, which is no item of docs/defects.md" % (rel, tok))

for f in faults:
    print("defects-check: " + f)
sys.exit(1 if faults else 0)
PY
