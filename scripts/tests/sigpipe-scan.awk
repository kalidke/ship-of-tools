# sigpipe-scan.awk FILE... -- prints FILE:LINE:TEXT for each pipeline whose reader can stop before its writer has
# finished: a stage that is head, read, grep -q or -m, sed with a q command, or awk with an exit. Under pipefail the
# writer's SIGPIPE then fails the pipeline at random, as install.sh's `printf | awk '... exit'` did (exit 141). Read the
# whole input instead (sed -n 1p, grep >/dev/null) or take it from a here-string. A continuation line, a leading `|`
# and an open single quote join the line before them. Run by under-bash.sh; proved by test-sigpipe-scan.sh.
function masked(s,    n, a, i, r) {  # a `|` inside single quotes is text (a regex alternation), not a pipe
    n = split(s, a, "'")
    r = a[1]
    for (i = 2; i <= n; i++) { if (i % 2 == 0) gsub(/\|/, "\001", a[i]); r = r "'" a[i] }
    return r
}
function flush() {
    if (buf != "" && masked(buf) ~ re) print FILENAME ":" start ":" buf
    buf = ""
}
function quotes(s) { return gsub(/'/, "'", s) % 2 }
BEGIN {
    re = "[^|]\\|[ \t]*(\\{ *)?(head|read|grep[^|]*[ \t]-[a-zA-Z]*[qm]|grep[^|]*--(quiet|max-count)"
    re = re "|sed[^|]*(;q|q;|[ '{]q[ '};]|q')|awk[^|]*exit)"
}
FNR == 1 { flush() }
/^[ \t]*#/ && !open { next }
{
    if (buf != "" && (open || buf ~ /[^|]\|[ \t]*$/ || buf ~ /\\[ \t]*$/ || $0 ~ /^[ \t]*\|[^|]/)) {
        buf = buf " " $0
    } else {
        flush(); buf = $0; start = FNR
    }
    open = quotes(buf)
}
END { flush() }
