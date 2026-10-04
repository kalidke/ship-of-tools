#!/usr/bin/env bash
# moved-check.sh: prove a commit range is a pure move. See README.md.
# Usage: moved-check.sh [--repo <dir>] [--allow-file <file>] <base>..<head>
exec python3 - "$@" <<'PY'
import os, re, subprocess, sys
from collections import Counter, defaultdict

def usage(msg=None):
    if msg:
        print("moved-check: " + msg, file=sys.stderr)
    print("usage: moved-check.sh [--repo <dir>] [--allow-file <file>] <base>..<head>", file=sys.stderr)
    sys.exit(2)

args = sys.argv[1:]
repo, allow_path, rng = os.getcwd(), None, None
i = 0
while i < len(args):
    a = args[i]
    if a == "--repo" and i + 1 < len(args):
        repo = args[i + 1]; i += 2
    elif a == "--allow-file" and i + 1 < len(args):
        allow_path = args[i + 1]; i += 2
    elif a.startswith("-") or rng is not None:
        usage("bad argument: " + a)
    else:
        rng = a; i += 1
if rng is None or ".." not in rng or rng.count("..") != 1 or "..." in rng:
    usage("need a <base>..<head> range")

allow = []
if allow_path:
    try:
        for ln in open(allow_path, encoding="utf-8"):
            ln = ln.rstrip("\n")
            if ln.strip():
                allow.append(re.compile(ln))
    except (OSError, re.error) as e:
        usage("allow file: %s" % e)

p = subprocess.run(["git", "-c", "core.quotepath=false", "diff", "-U0", "--no-renames",
                    "--no-color", "--no-ext-diff", rng],
                   cwd=repo, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
if p.returncode != 0:
    print("moved-check: git diff failed: " + p.stderr.decode("utf-8", "replace").strip(), file=sys.stderr)
    sys.exit(2)

VIS = re.compile(r"^pub(\([^)]*\))?\s+")
def norm(s):
    s = s.strip()
    return VIS.sub("", s, count=1).strip()

USE_OPEN = re.compile(r"^(pub(\([^)]*\))?\s+)?use\b[^;]*\{[^;}]*$")

# entries: (side, file, lineno, text, normalized, in_use_block)
removed, added = [], []
table = {}
cur = None
in_hunk = False
old_n = new_n = 0
use_state = {"-": False, "+": False}
for raw in p.stdout.decode("utf-8", "replace").split("\n"):
    if raw.startswith("diff --git "):
        in_hunk = False; cur = None
        continue
    if not in_hunk:
        if raw.startswith("--- "):
            pth = raw[4:].split("\t")[0]
            if pth != "/dev/null":
                cur = pth[2:] if pth.startswith("a/") else pth
            else:
                cur = None
            continue
        if raw.startswith("+++ "):
            pth = raw[4:].split("\t")[0]
            if pth != "/dev/null":
                cur = pth[2:] if pth.startswith("b/") else pth
            if cur is not None:
                table.setdefault(cur, [0, 0])
            continue
    m = re.match(r"^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@", raw)
    if m:
        in_hunk = True
        old_n, new_n = int(m.group(1)), int(m.group(2))
        use_state = {"-": False, "+": False}
        continue
    if not in_hunk or cur is None or raw == "" or raw[0] not in "+-":
        continue
    side, text = raw[0], raw[1:]
    n = norm(text)
    blk = False
    if cur.endswith(".rs"):
        if use_state[side]:
            blk = True
            if ";" in n:
                use_state[side] = False
        elif USE_OPEN.match(n):
            blk = True
            use_state[side] = True
    if side == "-":
        table[cur][1] += 1
        removed.append((cur, old_n, text.strip(), n, blk)); old_n += 1
    else:
        table[cur][0] += 1
        added.append((cur, new_n, text.strip(), n, blk)); new_n += 1

def is_claude(f):
    return os.path.basename(f) == "CLAUDE.md"

scaffold = 0
def take_claude(lst):
    global scaffold
    keep = []
    for e in lst:
        if is_claude(e[0]):
            scaffold += 1
        else:
            keep.append(e)
    return keep
removed, added = take_claude(removed), take_claude(added)
removed = [e for e in removed if e[3]]
added = [e for e in added if e[3]]

rc, ac = Counter(e[3] for e in removed), Counter(e[3] for e in added)
moved = sum(min(c, ac[k]) for k, c in rc.items() if k in ac)

def residual(lst, other):
    # the first min(count) occurrences of a line are the moved ones
    seen = Counter(); out = []
    for e in lst:
        seen[e[3]] += 1
        if seen[e[3]] > other.get(e[3], 0):
            out.append(e)
    return out
res_add, res_rem = residual(added, rc), residual(removed, ac)

SH_COMMENT = re.compile(r"^#.*(/|\b\w[\w.-]*\.(sh|rs|jl|ps1|md|toml|json)\b)")
RS = [re.compile(x) for x in (
    r"^mod\s+\w+;$",
    r"^#\[cfg\(.*\)\]$", r'^#\[path\s*=\s*".*"\]$',
    r"^use\s.*;$",
    r"^impl(<.*>)?\s+[\w:<>, '&]+(\s+for\s+[\w:<>, '&]+)?\s*\{$",
    r"^[{}]$", r"^//!", r"^#!\[.*\]$")]
SH = [re.compile(x) for x in (r"^(source|\.)\s+\S+", r"^#!")]
JL = [re.compile(x) for x in (r'^include\(".*"\)$', r"^module\s+\w+$", r"^end$")]

def is_scaffold(e):
    f, _, t, n, blk = e
    if blk:
        return True
    if n.startswith("#!"):
        return True
    if f.endswith(".rs"):
        return any(r.search(n) for r in RS)
    if f.endswith(".jl"):
        return any(r.search(n) for r in JL)
    if f.endswith(".sh") or f.endswith(".bash") or "." not in os.path.basename(f):
        return any(r.search(n) for r in SH) or bool(SH_COMMENT.search(n))
    return False

def filt(lst):
    global scaffold
    keep = []
    for e in lst:
        if is_scaffold(e):
            scaffold += 1
        else:
            keep.append(e)
    return keep
res_add, res_rem = filt(res_add), filt(res_rem)

by_file = 0
def split_allowed(lst):
    global by_file
    keep = []
    for e in lst:
        if any(r.search(e[3]) or r.search("%s:%s" % (e[0], e[3])) for r in allow):
            by_file += 1
        else:
            keep.append(e)
    return keep
res_add, res_rem = split_allowed(res_add), split_allowed(res_rem)

w = max([len(f) for f in table] + [4])
print("%-*s  %7s  %7s" % (w, "path", "added", "removed"))
for f in sorted(table):
    print("%-*s  %7d  %7d" % (w, f, table[f][0], table[f][1]))
print("MOVED: %d" % moved)
print("SCAFFOLD: %d" % scaffold)
if allow_path:
    print("ALLOWED-BY-FILE: %d" % by_file)
print("RESIDUAL-ADDED: %d" % len(res_add))
for f, n, t, _, _ in res_add:
    print("+ %s:%d: %s" % (f, n, t))
print("RESIDUAL-REMOVED: %d" % len(res_rem))
for f, n, t, _, _ in res_rem:
    print("- %s:%d: %s" % (f, n, t))
sys.exit(0 if not res_add and not res_rem else 1)
PY
