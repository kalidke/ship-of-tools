#!/usr/bin/env bash
# check-layout.sh: folder/file layout gate for the organize pass. See README.md.
# Usage: check-layout.sh [--repo <dir>] [--allow <file>] [--exempt <file>] [<folder> ...]
#        check-layout.sh [--repo <dir>] --report [<folder> ...]
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" exec python3 - "$@" <<'PY'
import os, re, subprocess, sys

FILE_LIMIT, FOLDER_LINES, FOLDER_FILES = 800, 3000, 12
NAMED_PATH_FILES = ["docs/ownership.md", "docs/integration.md", "CLAUDE.md"]   # pages whose backticked repo paths must exist
SRC_EXT = (".rs", ".jl", ".sh", ".ps1")

def usage(msg=None):
    if msg:
        print("check-layout: " + msg, file=sys.stderr)
    print("usage: check-layout.sh [--repo <dir>] [--allow <file>] [--exempt <file>] [<folder> ...]\n"
          "       check-layout.sh [--repo <dir>] --report [<folder> ...]", file=sys.stderr)
    sys.exit(2)

args = sys.argv[1:]
repo, allow_path, report, folders = os.getcwd(), None, False, []
exempt_path, exempt_explicit = os.path.join(os.environ.get("SELF_DIR", "."), "exempt.txt"), False
i = 0
while i < len(args):
    a = args[i]
    if a == "--repo" and i + 1 < len(args):
        repo = args[i + 1]; i += 2
    elif a == "--allow" and i + 1 < len(args):
        allow_path = args[i + 1]; i += 2
    elif a == "--exempt" and i + 1 < len(args):
        exempt_path, exempt_explicit = args[i + 1], True; i += 2
    elif a == "--report":
        report = True; i += 1
    elif a.startswith("-"):
        usage("bad argument: " + a)
    else:
        folders.append(a.strip("/") or "."); i += 1
folders = [os.path.normpath(f) for f in folders]

def git(*a):
    p = subprocess.run(["git", "-c", "core.quotepath=false"] + list(a), cwd=repo,
                       stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if p.returncode != 0:
        print("check-layout: git %s failed: %s" % (a[0], p.stderr.decode("utf-8", "replace").strip()),
              file=sys.stderr)
        sys.exit(2)
    return p.stdout
root = git("rev-parse", "--show-toplevel").decode().strip()
repo = root
tracked = [f for f in git("ls-files", "-z").decode("utf-8", "replace").split("\0") if f]
tracked = [f for f in tracked if os.path.isfile(os.path.join(root, f)) and not os.path.islink(os.path.join(root, f))]

allow = {}
if allow_path:
    try:
        for ln in open(allow_path, encoding="utf-8"):
            ln = ln.split("#", 1)[0].strip()
            if not ln:
                continue
            parts = ln.split(None, 2)
            if len(parts) < 2:
                usage("allow line needs '<kind> <path> <reason>': " + ln)
            allow[(parts[0], parts[1].rstrip("/") or ".")] = parts[2] if len(parts) > 2 else ""
    except OSError as e:
        usage("allow file: %s" % e)

def glob_re(g):
    """Glob to regex: ** crosses folders, * and ? stay inside one name; a trailing /** also matches the folder."""
    out, k = "", 0
    while k < len(g):
        if g.startswith("**/", k):
            out += "(?:.*/)?"; k += 3
        elif g.startswith("/**", k) and k + 3 == len(g):
            out += "(?:/.*)?"; k += 3
        elif g.startswith("**", k):
            out += ".*"; k += 2
        elif g[k] == "*":
            out += "[^/]*"; k += 1
        elif g[k] == "?":
            out += "[^/]"; k += 1
        else:
            out += re.escape(g[k]); k += 1
    return re.compile("^" + out + "$")

exempt = []   # (kind, regex, reason)
try:
    for ln in open(exempt_path, encoding="utf-8"):
        ln = ln.split("#", 1)[0].strip()
        if not ln:
            continue
        parts = ln.split(None, 2)
        if len(parts) < 2 or parts[0] not in ("no-page", "file-size", "folder-size", "file-list", "named-path"):
            usage("bad exempt line: " + ln)
        exempt.append((parts[0], glob_re(parts[1]), parts[2] if len(parts) > 2 else ""))
except FileNotFoundError:
    if exempt_explicit:
        usage("exempt file not found: " + exempt_path)
except OSError as e:
    usage("exempt file: %s" % e)

def read(f):
    with open(os.path.join(root, f), "rb") as fh:
        return fh.read().decode("utf-8", "replace")

def is_source(f):
    if f.endswith(SRC_EXT):
        return True
    if "." in os.path.basename(f):
        return False
    with open(os.path.join(root, f), "rb") as fh:
        return fh.read(2) == b"#!"

def is_test_file(f):
    b = os.path.basename(f)
    if "tests" in f.split("/")[:-1]:
        return True
    return f.endswith(".rs") and (b.endswith("_tests.rs") or b == "tests.rs"
                                  or b.startswith("tests_") or "_tests_" in b
                                  or b == "test_support.rs")

def requires_test(pred):
    """True when a cfg predicate holds only if `test` is set: test, or all(...) with such an operand."""
    pred = pred.strip()
    if pred == "test":
        return True
    m = re.match(r"^all\((.*)\)$", pred, re.S)
    if not m:
        return False
    depth, cur, ops = 0, "", []
    for ch in m.group(1):
        if ch == "," and depth == 0:
            ops.append(cur); cur = ""
            continue
        depth += (ch == "(") - (ch == ")")
        cur += ch
    ops.append(cur)
    return any(requires_test(o) for o in ops)

def is_test_cfg(attr):
    m = re.match(r"^#\[\s*cfg\s*\((.*)\)\s*\]\s*(//.*)?$", attr.strip(), re.S)
    return bool(m) and requires_test(m.group(1))

def counts(f):
    """(code_lines, test_lines) of a source file."""
    lines = read(f).splitlines()
    if is_test_file(f):
        return 0, len(lines)
    if f.endswith(".rs"):
        test, k = 0, 0
        while k < len(lines):
            if not lines[k].startswith("#["):
                k += 1
                continue
            j = k
            while j < len(lines) and (not lines[j].strip() or lines[j].lstrip().startswith("#[")):
                j += 1
            if (j < len(lines) and re.match(r"^(pub(\([^)]*\))?\s+)?mod\s", lines[j])
                    and any(is_test_cfg(a) for a in lines[k:j] if a.startswith("#["))):
                end = j
                if not lines[j].rstrip().endswith(";"):
                    end = j + 1
                    while end < len(lines) and lines[end] != "}":
                        end += 1
                end = min(end, len(lines) - 1)
                test += end - k + 1
                k = end + 1
            else:
                k = j
        return len(lines) - test, test
    return len(lines), 0

def folder_of(f):
    return os.path.dirname(f) or "."

sources = [f for f in tracked if is_source(f)]
by_folder = {}
for f in sources:
    by_folder.setdefault(folder_of(f), []).append(f)
def own_entries(d):
    pre = "" if d == "." else d + "/"
    names = set()
    for f in tracked:
        if f.startswith(pre):
            rest = f[len(pre):]
            if rest != "CLAUDE.md":
                names.add(rest.split("/")[0] + "/" if "/" in rest else rest)
    return names

tracked_set = set(tracked)
covered = {}   # covered folder -> its root folder
def _join(d, n):
    return n if d == "." else d + "/" + n
for f in tracked:
    b, d = os.path.basename(f), folder_of(f)
    subs = []
    if b == "Cargo.toml" and re.search(r"(?m)^\[package\]", read(f)):
        subs = ["src"]
    elif b == "Project.toml":
        subs = ["src", "test", "ext"]
    for sub in subs:
        c = _join(d, sub)
        if any(t.startswith(c + "/") for t in tracked) and _join(c, "CLAUDE.md") not in tracked_set:
            covered[c] = d

def entries(d):
    names = own_entries(d)
    for c, r in covered.items():
        if r == d:
            names.discard(c[len(d) + 1:] + "/" if d != "." else c + "/")
            names |= {c[len(d) + 1:] + "/" + n if d != "." else c + "/" + n for n in own_entries(c)}
    return names

if not folders:
    paged = {folder_of(f) for f in tracked if os.path.basename(f) == "CLAUDE.md"}
    folders = sorted(set(by_folder) | paged | {r for c, r in covered.items() if c in by_folder})

if report:
    for d in folders:
        fs = by_folder.get(d, [])
        print(d)
        rows = sorted(((counts(f), f) for f in fs), key=lambda r: (-(r[0][0] + r[0][1]), r[1]))
        for (c, t), f in sorted(rows, key=lambda r: (-r[0][0], -r[0][1], r[1])):
            print("  code %5d  test %5d  %s" % (c, t, os.path.basename(f)))
    sys.exit(0)

violations = []  # (kind, path, detail)
def add(kind, path, detail):
    violations.append((kind, path, detail))

def listed(page):
    names, inside, found = [], False, False
    for ln in read(page).splitlines():
        if ln.startswith("## "):
            inside = ln.strip() == "## Files"
            found = found or inside
            continue
        if inside and ln.startswith("- "):
            m = re.search(r"`([^`]+)`", ln)
            if m:
                names.append(m.group(1))
    return found, set(names)

for d in folders:
    fs = by_folder.get(d, [])
    pre = "" if d == "." else d + "/"
    page = pre + "CLAUDE.md"
    has_page = page in tracked
    tests_free = [f for f in fs if not is_test_file(f)]
    for f in sorted(fs):
        c, t = counts(f)
        if c + t > FILE_LIMIT if is_test_file(f) else c > FILE_LIMIT:
            add("file-size", f, "%d %s lines (limit %d)" % (c + t if is_test_file(f) else c,
                "test" if is_test_file(f) else "code", FILE_LIMIT))
    cov = sorted(c for c, r in covered.items() if r == d and c in by_folder)
    if not has_page and ((fs and d not in covered) or cov):
        add("no-page", d, "%s%s, no CLAUDE.md" % ("%d source files" % len(fs) if fs and d not in covered else "",
            (" and " if fs and d not in covered else "") + "covers " + ", ".join(cov) if cov else ""))
    if has_page:
        found, names = listed(page)
        want = entries(d)
        if not found:
            add("file-list", page, "no '## Files' section; missing: " + ", ".join(sorted(want)))
        elif names != want:
            parts = []
            if want - names:
                parts.append("missing: " + ", ".join(sorted(want - names)))
            if names - want:
                parts.append("extra: " + ", ".join(sorted(names - want)))
            add("file-list", page, "; ".join(parts))
    total = sum(counts(f)[0] for f in tests_free)
    if total > FOLDER_LINES or len(tests_free) > FOLDER_FILES:
        add("folder-size", d, "%d source files, %d code lines (limits %d files, %d lines)"
            % (len(tests_free), total, FOLDER_FILES, FOLDER_LINES))

# named-path: every backticked repo path in a listed page names a tracked file or a folder holding tracked files.
def named_paths(page):
    """Yield the checkable paths of a page: inline backticks outside fenced blocks, trailing :n, :n-m and ::item
    stripped, one {a,b} group expanded; tokens with a placeholder character or a space are skipped."""
    fenced = False
    for ln in read(page).splitlines():
        if ln.lstrip().startswith("```"):
            fenced = not fenced
            continue
        if fenced:
            continue
        for tok in re.findall(r"`([^`]+)`", ln):
            if re.search(r"[<>*$~ ]", tok):
                continue
            tok = re.sub(r"::.*$", "", tok)
            tok = re.sub(r":\d+(-\d+)?$", "", tok)
            m = re.match(r"^([^{}]*)\{([^{}]*)\}([^{}]*)$", tok)
            for t in ([m.group(1) + x + m.group(3) for x in m.group(2).split(",")] if m else [tok]):
                yield t.rstrip("/")

top_level = {f.split("/")[0] for f in tracked}
named_checked = 0
named_listed = [p for p in NAMED_PATH_FILES if p in tracked_set]
for page in named_listed:
    for t in named_paths(page):
        if not t or t.split("/")[0] not in top_level:
            continue
        named_checked += 1
        if t not in tracked_set and not any(f.startswith(t + "/") for f in tracked):
            add("named-path", page, t)

nv = na = ne = 0
used = set()
for kind, path, detail in violations:
    if any(k == kind and r.match(path) for k, r, _ in exempt):
        ne += 1
    elif (kind, path) in allow:
        na += 1
        used.add((kind, path))
        print("ALLOWED %s %s %s [%s]" % (kind, path, detail, allow[(kind, path)]))
    else:
        nv += 1
        print("VIOLATION %s %s %s" % (kind, path, detail))
unused = [k for k in allow if k not in used]
for kind, path in unused:
    print("UNUSED-ALLOW %s %s %s" % (kind, path, allow[(kind, path)]))
if named_listed:
    print("named-path: %d tokens checked in %d files" % (named_checked, len(named_listed)))
print("violations: %d, allowed: %d, exempt: %d, unused-allow: %d, folders checked: %d"
      % (nv, na, ne, len(unused), len(folders)))
sys.exit(1 if nv or unused else 0)
PY
