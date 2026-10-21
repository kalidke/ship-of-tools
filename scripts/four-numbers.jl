#!/usr/bin/env julia
# four-numbers.jl: the four numbers every reorganization pass reports, per subsystem, read from git.
# Usage: julia --startup-file=no scripts/four-numbers.jl [--names] [--unmapped] [<git rev>]
# Measures a commit (git ls-tree and cat-file), not the working tree. Standard library only.
const VERSION = "four-numbers 1"

# Path prefix -> subsystem; the longest prefix wins. Tests of the two integration folders are assigned by file below.
const SUBSYSTEMS = ["topology", "wire", "server", "lifecycle", "rows", "agents", "platform", "capsule", "vt100",
                    "messaging", "files", "sidecars", "pages", "fe-ui", "fe-net", "distribution", "records", "test-support"]
const PREFIXES = [
    "rust/protocol/src/topology/" => "topology", "rust/backend/src/topology" => "topology",
    "rust/protocol/" => "wire",
    "rust/backend/src/daemon_log" => "server", "rust/backend/src/durable.rs" => "platform",
    "rust/backend/src/paths.rs" => "platform", "rust/frontend/build.rs" => "fe-ui",
    "rust/frontend/src/browser_open.rs" => "fe-ui", "rust/frontend/src/relaunch.rs" => "distribution",
    "rust/backend/src/server/" => "server", "rust/backend/src/main.rs" => "server",
    "rust/backend/src/clients" => "server", "rust/backend/src/session.rs" => "server",
    "rust/backend/src/lifecycle/" => "lifecycle", "rust/frontend/src/lease" => "lifecycle",
    "rust/backend/src/rows/" => "rows",
    "agents/" => "agents", "rust/backend/src/agents/" => "agents",
    "rust/log/src/host/" => "platform", "rust/log/src/identity/" => "platform",
    "rust/log/" => "capsule", "julia/sotlog/" => "capsule",
    "rust/vt100/" => "vt100",
    "comm/" => "messaging", "rust/backend/src/comm/" => "messaging",
    "rust/backend/src/files/" => "files",
    "rust/backend/src/sidecars/" => "sidecars", "rust/backend/sidecars/" => "sidecars", "julia/" => "sidecars",
    "core/" => "sidecars",
    "rust/backend/src/pages/" => "pages", "rust/frontend/src/pages" => "pages",
    "rust/frontend/src/ui/" => "fe-ui", "rust/frontend/src/main.rs" => "fe-ui", "rust/frontend/src/cli.rs" => "fe-ui",
    "rust/frontend/src/net/" => "fe-net",
    "scripts/" => "distribution", "rust/updater/" => "distribution", "rust/backend/src/update" => "distribution",
    "rust/frontend/src/selfupdate" => "distribution", "src/" => "distribution", "test/" => "distribution",
    ".github/" => "distribution", "deploy/" => "distribution",
    "docs/" => "records",
]
# The integration tests, assigned by file stem as the package page's "Integration tests" section lists them.
const TEST_STEMS = Dict{String,Vector{String}}(
    "distribution" => ["shell_dial", "unit_pin", "same_version", "window_start"],
    "rows" => ["capsule_workspaces"],
    "messaging" => ["comm_wake", "comm_file", "comm_send", "hub_link"],
    "server" => ["daemon_lifetime", "admission", "control_session", "ping_reaper", "live_socket", "daemon_boot",
                 "daemon_stdout", "subcommand_help", "support_checks", "active_frontend", "stdio_bridge",
                 "status_integration"],
    "lifecycle" => ["window_lease", "contained_job", "ancestors"],
    "topology" => ["topology_set", "topology_sync", "relay_refresh"],
    "platform" => ["challenge_unix", "challenge_macos", "connect_own", "other_account", "winhandle_windows",
                   "macos_kernel_facts"],
    "capsule" => ["lane_bridge", "switch_latency", "keystroke_latency", "socket_unix", "pipe_win", "wire", "e2e_socket",
                  "e2e_pipe", "capsule", "supervisor", "fault_storage", "fault_kill", "fault_terminate",
                  "reconcile_matrix", "golden", "conpty", "fe_client", "attach_worker", "claude_e2e", "claude_rig"],
    "sidecars" => ["repl_windows", "child_folders"],
    "pages" => ["site_root", "site_windows"],
    "files" => ["preview_order", "old_watcher"],
    "agents" => ["trust_declare", "trust_premises", "agent_exec"],
)
const STEM_SUBSYSTEM = Dict(s => sub for (sub, v) in TEST_STEMS for s in v)
const TEST_DIRS = ("rust/backend/tests/", "rust/log/tests/")

# Rust crates by name as `use` paths spell them, to the folder of their source.
const CRATE_DIR = Dict("sot_protocol" => "rust/protocol", "sot_updater" => "rust/updater", "sot_log" => "rust/log",
                       "sot_backend" => "rust/backend", "sot_frontend" => "rust/frontend")
const CODE_EXT = (".rs", ".jl", ".sh", ".ps1", ".ts", ".js", ".mjs", ".py")
const GENERIC = Set(["main", "new", "run", "default", "fmt", "from", "drop", "clone", "eq", "hash", "parse", "usage",
                     "die", "log"])

git(args...) = read(`git $args`)

function subsystem(path)
    if (d = findfirst(t -> startswith(path, t), TEST_DIRS)) !== nothing
        rest = path[length(TEST_DIRS[d])+1:end]
        startswith(rest, "support/") && return "test-support"
        return get(STEM_SUBSYSTEM, replace(first(split(rest, '/')), r"\.rs$" => ""), nothing)
    end
    best, len = nothing, 0
    for (p, s) in PREFIXES
        startswith(path, p) && length(p) > len && ((best, len) = (s, length(p)))
    end
    best
end

function is_test_file(path)
    parts = split(path, '/')
    any(p -> p in ("tests", "test", "fixtures"), parts[1:end-1]) && return true
    occursin(r"(^test[_-]|[_-]tests?\.[a-z0-9]+$|^runtests\.jl$)", parts[end])
end

# A same-length copy of Rust source with comments, strings and char literals blanked (newlines kept).
function mask_rust(text)
    c = collect(text); n = length(c); m = copy(c); i = 1
    blank(a, b) = for k in a:b; m[k] != '\n' && (m[k] = ' '); end
    while i <= n
        ch = c[i]
        if ch == '/' && i < n && c[i+1] == '/'
            j = i; while j <= n && c[j] != '\n'; j += 1; end
            blank(i, j - 1); i = j
        elseif ch == '/' && i < n && c[i+1] == '*'
            depth, j = 1, i + 2
            while j <= n && depth > 0
                if c[j] == '/' && j < n && c[j+1] == '*'; depth += 1; j += 2
                elseif c[j] == '*' && j < n && c[j+1] == '/'; depth -= 1; j += 2
                else j += 1 end
            end
            blank(i, min(j - 1, n)); i = j
        elseif ch == 'r' && i < n && (c[i+1] == '"' || c[i+1] == '#') && (i == 1 || !(isletter(c[i-1]) || isnumeric(c[i-1]) || c[i-1] == '_'))
            j = i + 1; h = 0
            while j <= n && c[j] == '#'; h += 1; j += 1; end
            if j <= n && c[j] == '"'
                j += 1
                while j <= n && !(c[j] == '"' && all(k -> j + k <= n && c[j+k] == '#', 1:h)); j += 1; end
                e = min(j + h, n); blank(i, e); i = e + 1
            else
                i += 1
            end
        elseif ch == '"'
            j = i + 1
            while j <= n && c[j] != '"'; c[j] == '\\' && (j += 1); j += 1; end
            e = min(j, n); blank(i, e); i = e + 1
        elseif ch == '\''
            if i + 2 <= n && c[i+1] == '\\'
                j = i + 2; while j <= n && c[j] != '\''; j += 1; end
                e = min(j, n); blank(i, e); i = e + 1
            elseif i + 2 <= n && c[i+2] == '\''
                blank(i, i + 2); i += 3
            else
                i += 1
            end
        else
            i += 1
        end
    end
    String(m)
end

# Character ranges of items gated by an inline `#[cfg(test)]` (attribute through the item's end), on masked text.
function test_ranges(masked)
    c = collect(masked); n = length(c); out = UnitRange{Int}[]
    starts = [m.offset for m in eachmatch(r"#\[cfg\((?:all\()?test\b", masked)]
    # eachmatch indexes are byte offsets; masked is mostly ASCII, so map through char positions.
    cpos = Dict{Int,Int}()
    for (ci, bi) in enumerate(eachindex(masked)); cpos[bi] = ci; end
    last_end = 0
    for b in starts
        i = cpos[b]; i <= last_end && continue
        j = i; depth = 0
        while j <= n   # skip the attributes, then walk to the item's `{` ... `}` or `;`
            if c[j] == '#' && j < n && c[j+1] == '['
                d = 0
                while j <= n; c[j] == '[' && (d += 1); c[j] == ']' && (d -= 1); j += 1; d == 0 && break; end
            elseif isspace(c[j]); j += 1
            else break end
        end
        while j <= n
            ch = c[j]
            if ch in ('(', '['); depth += 1
            elseif ch in (')', ']'); depth -= 1
            elseif ch == ';' && depth == 0; break
            elseif ch == '{' && depth == 0
                d = 0
                while j <= n; c[j] == '{' && (d += 1); c[j] == '}' && (d -= 1); d == 0 && break; j += 1; end
                break
            end
            j += 1
        end
        e = min(j, n); push!(out, i:e); last_end = e
    end
    out
end

mutable struct Row; product::Int; test::Int; files::Int; end
const KINDS = Dict{String,Int}()   # test characters by kind: inline, named test file, integration folder

function main(args)
    show_names = "--names" in args; show_unmapped = "--unmapped" in args
    revs = filter(a -> !startswith(a, "--"), args)
    rev = isempty(revs) ? "HEAD" : revs[1]
    t0 = time()
    println(VERSION, " at ", strip(String(git("rev-parse", "--short=9", rev))))
    entries = Tuple{String,String}[]   # (sha, path) of code files
    for line in split(String(git("ls-tree", "-r", "-z", rev)), '\0'; keepempty=false)
        meta, path = split(line, '\t'; limit=2)
        any(e -> endswith(path, e), CODE_EXT) && push!(entries, (split(meta, ' ')[3], String(path)))
    end
    files = Set(p for (_, p) in entries)
    stats = Dict(s => Row(0, 0, 0) for s in SUBSYSTEMS)
    unmapped = Dict{String,Int}()
    defs = Dict{String,Dict{String,Int}}()      # name -> file -> definitions (non-test)
    refs = Dict(s => Set{String}() for s in SUBSYSTEMS)
    resolve_cache = Dict{Tuple{String,String},Union{String,Nothing}}()

    function module_subsystem(base, mod)
        get!(resolve_cache, (base, mod)) do
            for cand in ("$base/src/$mod.rs", "$base/src/$mod/mod.rs")
                cand in files && return subsystem(cand)
            end
            any(p -> startswith(p, "$base/src/$mod/"), files) && return subsystem("$base/src/$mod/x.rs")
            for root in ("lib.rs", "main.rs")
                "$base/src/$root" in files && return subsystem("$base/src/$root")
            end
            nothing
        end
    end

    proc = open(`git cat-file --batch`, "r+")
    function blob(sha)
        println(proc, sha); flush(proc)
        _, _, size = split(readline(proc))
        data = read(proc, parse(Int, size)); read(proc, UInt8)
        s = String(data); isvalid(s) ? s : join(c for c in s if isvalid(c))
    end

    for (sha, path) in entries
        sub = subsystem(path)
        if sub === nothing
            unmapped[path] = 1; continue
        end
        text = blob(sha); st = stats[sub]; st.files += 1
        test_file = is_test_file(path)
        total = length(text); test_chars = test_file ? total : 0
        body = text   # the non-test text, masked for Rust
        if endswith(path, ".rs")
            masked = mask_rust(text)
            if test_file || occursin(r"#!\[cfg\(test\)\]", masked)
                test_chars = total; body = ""
            else
                ranges = test_ranges(masked)
                if !isempty(ranges)
                    tc = collect(masked); keep = trues(length(tc))
                    for r in ranges; keep[r] .= false; end
                    test_chars = count(!, keep); body = String(tc[keep])
                else
                    body = masked
                end
            end
        elseif test_file
            body = ""
        end
        st.test += test_chars; st.product += total - test_chars
        if test_chars > 0
            kind = startswith(path, TEST_DIRS[1]) || startswith(path, TEST_DIRS[2]) ? "integration" : test_file ? "test files" : "inline"
            KINDS[kind] = get(KINDS, kind, 0) + test_chars
        end
        isempty(body) && continue
        # names defined in non-test code
        names = String[]
        if endswith(path, ".rs")
            for m in eachmatch(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)", body); push!(names, m.captures[1]); end
        elseif endswith(path, ".sh")
            for m in eachmatch(r"^[ \t]*(?:function[ \t]+)?([A-Za-z_][A-Za-z0-9_:.-]*)[ \t]*\(\)"m, body); push!(names, m.captures[1]); end
            for m in eachmatch(r"^[ \t]*function[ \t]+([A-Za-z_][A-Za-z0-9_:.-]*)"m, body); push!(names, m.captures[1]); end
        elseif endswith(path, ".ps1")
            for m in eachmatch(r"^[ \t]*function[ \t]+([A-Za-z_][A-Za-z0-9_:.-]*)"mi, body); push!(names, m.captures[1]); end
        elseif endswith(path, ".jl")
            for m in eachmatch(r"^[ \t]*function[ \t]+(?:[A-Za-z_][A-Za-z0-9_]*\.)*([A-Za-z_][A-Za-z0-9_!]*)"m, body); push!(names, m.captures[1]); end
            for m in eachmatch(r"^(?:[A-Za-z_][A-Za-z0-9_]*\.)*([A-Za-z_][A-Za-z0-9_!]*)\([^\n]*\)[^\n=]*=(?!=)"m, body); push!(names, m.captures[1]); end
        end
        for nm in unique(names)
            (length(nm) < 4 || nm in GENERIC) && continue
            d = get!(defs, nm, Dict{String,Int}()); d[path] = count(==(nm), names)
        end
        # cross-subsystem module references, non-test Rust only
        if endswith(path, ".rs") && sub != "vt100"
            mm = match(r"^(rust/[a-z0-9]+)/src/", path)
            mm === nothing && continue
            own = String(mm.captures[1]); isbin = occursin("/src/bin/", path)
            for m in eachmatch(r"\b(crate|sot_[a-z]+)::(\{[^;]*?\}|[A-Za-z_][A-Za-z0-9_]*)", body)
                head = m.captures[1]
                base = head == "crate" ? (isbin ? nothing : own) : get(CRATE_DIR, head, nothing)
                base === nothing && continue
                tail = m.captures[2]
                mods = tail[1] == '{' ? [x.captures[1] for x in eachmatch(r"(?:^|[{,])\s*([A-Za-z_][A-Za-z0-9_]*)\s*(?:::\s*(?:\{[^}]*\}|[A-Za-z0-9_*]+)\s*)?(?=,|\})"s, tail)] : [tail]
                for md in mods
                    t = module_subsystem(base, md)
                    t !== nothing && t != sub && push!(refs[sub], "$base::$md")
                end
            end
        end
    end
    close(proc)

    twice = Dict(n => d for (n, d) in defs if length(d) >= 2)
    twice_by_sub = Dict(s => 0 for s in SUBSYSTEMS)
    for (n, d) in twice, s in Set(subsystem(p) for p in keys(d)); s !== nothing && (twice_by_sub[s] += 1); end

    k(x) = x ÷ 4000
    println(rpad("subsystem", 14), lpad("product k", 10), lpad("test k", 8), lpad("twice", 7), lpad("cross-refs", 12))
    tp = tt = tx = 0
    for s in SUBSYSTEMS
        r = stats[s]
        println(rpad(s, 14), lpad(k(r.product), 10), lpad(k(r.test), 8), lpad(twice_by_sub[s], 7), lpad(length(refs[s]), 12))
        if s != "vt100"; tp += r.product; tt += r.test; tx += length(refs[s]); end
    end
    println(rpad("total", 14), lpad(k(tp), 10), lpad(k(tt), 8), lpad(length(twice), 7), lpad(tx, 12),
            "   (vt100 excluded from the total; twice counts names once across the tree)")
    println("cross-refs (baseline; contract-named calls come with the crate): distinct crate::<module> and sot_<crate>::<module> paths of another subsystem")
    println("test tokens by kind (k): ", join(("$k $(v ÷ 4000)" for (k, v) in sort(collect(KINDS))), ", "))
    println("unmapped: ", length(unmapped), " code files")
    if show_unmapped
        for p in sort(collect(keys(unmapped))); println("  ", p); end
    end
    if show_names
        println("\nnames implemented twice: ", length(twice))
        for (n, d) in sort(collect(twice); by = x -> (-length(x[2]), x[1]))
            println("  ", n, " (", length(d), " files, ", sum(values(d)), " definitions): ", join(sort(collect(keys(d))), ", "))
        end
    end
    println("elapsed ", round(time() - t0; digits=1), " s")
end

main(ARGS)
