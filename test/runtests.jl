using ShipTools
using Test

# Path to the repo's comm/ source (tests run from test/, package root is ..).
const COMM_DIR = normpath(joinpath(@__DIR__, "..", "comm"))

@testset "Ship of Tools" begin
    @testset "codex hooks payload" begin
        # This file has died silently twice — an unrecognized top-level key
        # (codex rejects the WHOLE file) and a wrong event-key case (the key is
        # dropped). Neither reports an error anywhere: the only symptom is that
        # every codex session stops reporting its work-state, which reads as a
        # Ship of Tools bug rather than a parse failure. These checks are the
        # cheap standing guard; see comm/adapters/codex/hooks.README.md.
        hooks_path = joinpath(COMM_DIR, "adapters", "codex", "hooks.json")
        @test isfile(hooks_path)
        txt = read(hooks_path, String)

        # (1) Real JSON. Also covers the scanner's one blind spot: on malformed
        # input a stray key can sit at depth <= 0 and escape detection.
        @test_nowarn ShipTools._json_toplevel_keys(txt)
        if !isnothing(Sys.which("jq"))
            @test success(pipeline(`jq empty $hooks_path`; stderr = devnull))
        end

        # (2) EXACTLY one top-level key. The struct is deny_unknown_fields, so
        # anything else here silently disables every hook.
        @test ShipTools._json_toplevel_keys(txt) == ["hooks"]

        # (3) Event keys are PascalCase. serde(rename = "...") with no alias, in
        # 0.142.5 and 0.146 alike, so a snake_case key is an unrecognized inner
        # key and is dropped in silence.
        for ev in ("UserPromptSubmit", "PostToolUse", "Stop", "PermissionRequest")
            @test occursin("\"$ev\"", txt)
        end
        # A snake_case twin is not a harmless fallback: if it ever became a
        # serde alias, both keys present would be a fatal duplicate-field parse
        # error — the same total silent failure, reintroduced.
        for ev in ("user_prompt_submit", "post_tool_use", "permission_request")
            @test !occursin("\"$ev\"", txt)
        end

        # (4) Every hook command must be a script install_comm actually deploys
        # into $SOT_COMM_HOME/bin, so a rename can't leave the payload pointing
        # at a file that never arrives. Sources are searched across adapters on
        # purpose: three of the four scripts the CODEX payload references
        # (comm-status-{working,heartbeat,idle}.sh) ship with the CLAUDE
        # adapter, so `install_comm(clis = [:codex])` on its own would deploy
        # hooks pointing at scripts it never installed. Harmless under the
        # default clis = [:claude, :codex]; worth knowing before anyone splits
        # them.
        sources = String[]
        for (root, _, files) in walkdir(COMM_DIR), f in files
            endswith(f, ".sh") && push!(sources, f)
        end
        for m in eachmatch(r"\$HOME/\.sot-comm/bin/([A-Za-z0-9._-]+)", txt)
            @test m.captures[1] in sources
        end
    end

    @testset "_json_toplevel_keys" begin
        f = ShipTools._json_toplevel_keys
        @test f("{\"hooks\": {\"Stop\": []}}") == ["hooks"]
        # Format-independent: a regex over indented key lines silently stopped
        # matching when the file was reformatted, which is why this walks text.
        @test f("{\"hooks\":{\"Stop\":[]}}") == ["hooks"]
        @test f("{\n\t\"hooks\": {}\n}") == ["hooks"]
        # Stray keys are caught wherever they sit.
        @test f("{\"_comment\": \"x\", \"hooks\": {}}") == ["_comment", "hooks"]
        # Not fooled by structure inside string values.
        @test f("{\"hooks\": {\"S\": \"{a:b}\"}}") == ["hooks"]
        @test f("{\"hooks\": {\"S\": \"he said \\\"hi\\\": ok\"}}") == ["hooks"]
        # A depth-1 string VALUE must not be mistaken for a key.
        @test f("{\"hooks\": \"x\", \"y\": 1}") == ["hooks", "y"]
    end

    @testset "codex marketplace payload registry" begin
        # The overwrite guard for ~/.agents/plugins/marketplace.json is a
        # byte-compare against CODEX_MARKETPLACE_PAYLOADS. It used to be
        # `occursin("sot-local", ...)`, which rewrote WHOLESALE any file that
        # merely mentioned the string — including one carrying entries other
        # tools had added. These checks pin the registry the compare relies on.
        payloads = ShipTools.CODEX_MARKETPLACE_PAYLOADS
        @test !isempty(payloads)

        # Every entry is real JSON with the marketplace's top-level shape, and
        # the CURRENT one (what the writer emits) names our marketplace and
        # plugin. jq is the honest parser where available; the scanner check
        # runs everywhere.
        for txt in payloads
            @test sort(ShipTools._json_toplevel_keys(txt)) ==
                  ["interface", "name", "plugins"]
        end
        @test occursin("\"sot-local\"", first(payloads))
        @test occursin("\"sot-comm\"", first(payloads))
        if !isnothing(Sys.which("jq"))
            for txt in payloads
                @test success(pipeline(pipeline(IOBuffer(txt), `jq empty`); stderr = devnull))
            end
        end

        # The same-commit convention: a modified file must NOT match the
        # registry, or the guard would overwrite user additions. A plugin
        # appended to our own payload is the exact shape the old guard
        # destroyed.
        modified = replace(
            first(payloads),
            "  ]" => """    ,{ "name": "someone-elses-plugin" }\n  ]""",
        )
        @test modified != first(payloads)   # the replace really landed
        @test !(modified in payloads)
    end

    @testset "install_file: copy-then-rename" begin
        # install_file replaced every `cp(src, dst; force = true)` file-copy
        # site because that form removes dst BEFORE copying: a copy that then
        # fails leaves nothing at dst, not merely an un-updated file. This is
        # exactly how comm-relay.sh vanished from a live ~/.sot-comm/bin while
        # every other script updated normally (a Windows FE box, 2026-09-04).
        mktempdir() do dir
            # Plain success: dst gets src's content, no leftover temp file.
            src = joinpath(dir, "src.txt")
            write(src, "hello")
            dst = joinpath(dir, "dst.txt")
            ShipTools.install_file(src, dst)
            @test read(dst, String) == "hello"
            @test !any(n -> occursin(".tmp-", n), readdir(dir))

            # Failure with a PRE-EXISTING dst (the field scenario: an update
            # over a working install): the old dst must survive untouched,
            # and no temp file should linger.
            missing_src = joinpath(dir, "does-not-exist.txt")
            old_dst = joinpath(dir, "old.txt")
            write(old_dst, "OLD CONTENT")
            err = try
                ShipTools.install_file(missing_src, old_dst)
                nothing
            catch e
                e
            end
            @test err isa ErrorException
            @test read(old_dst, String) == "OLD CONTENT"
            @test !isfile(old_dst * ".tmp")
            # The error's FIRST line names dst and the underlying cause: a
            # launcher that only surfaces the tail of a crash's stderr must
            # still see the useful part, not just the last stack frame.
            firstline = split(err.msg, '\n'; limit = 2)[1]
            @test occursin(old_dst, firstline)
            @test occursin("no such file", lowercase(firstline)) ||
                  occursin("enoent", lowercase(firstline))

            # Failure with NO prior dst (a first install of that file): dst
            # must stay absent, not half-written.
            fresh_dst = joinpath(dir, "fresh.txt")
            @test !isfile(fresh_dst)
            @test_throws ErrorException ShipTools.install_file(missing_src, fresh_dst)
            @test !isfile(fresh_dst)
            @test !isfile(fresh_dst * ".tmp")

            # A genuine permission failure (closer to the field defect than a
            # missing source) tells the same story. Skipped if running as
            # root, where a mode of 0 still reads fine.
            unreadable_src = joinpath(dir, "unreadable.txt")
            write(unreadable_src, "secret")
            chmod(unreadable_src, 0o000)
            can_still_read = try
                read(unreadable_src, String)
                true
            catch
                false
            end
            if !can_still_read
                write(old_dst, "OLD CONTENT 2")
                err2 = try
                    ShipTools.install_file(unreadable_src, old_dst)
                    nothing
                catch e
                    e
                end
                @test err2 isa ErrorException
                @test read(old_dst, String) == "OLD CONTENT 2"
                fl2 = split(err2.msg, '\n'; limit = 2)[1]
                @test occursin(old_dst, fl2)
                @test occursin("denied", lowercase(fl2)) || occursin("eacces", lowercase(fl2))
            end
            chmod(unreadable_src, 0o644)  # let mktempdir clean up

            # A genuine RENAME failure (not a copy failure): src copies to
            # the temp path fine, but the publish step can't land — modeled
            # here by a pre-existing DIRECTORY at dst, which a file rename
            # can never replace. This is the failure mode that mattered most:
            # `mv(tmp, dst; force = true)` (what install_file used to call)
            # falls back to deleting dst and retrying on a failed plain
            # rename, so a stubborn destination — a live process still has
            # `comm-relay.sh` open, was the field case — got DELETED even
            # though the replace ultimately failed too. A bare rename must
            # never delete dst on failure.
            blocked_dst = joinpath(dir, "blocked")
            mkpath(blocked_dst)
            write(joinpath(blocked_dst, "marker.txt"), "keepme")
            rename_src = joinpath(dir, "new-content.txt")
            write(rename_src, "new content")
            err3 = try
                ShipTools.install_file(rename_src, blocked_dst)
                nothing
            catch e
                e
            end
            @test err3 isa ErrorException
            @test isdir(blocked_dst)  # untouched — not deleted, not replaced
            @test read(joinpath(blocked_dst, "marker.txt"), String) == "keepme"
            @test !isfile(blocked_dst * ".tmp")
            fl3 = split(err3.msg, '\n'; limit = 2)[1]
            @test occursin(blocked_dst, fl3)
        end
    end

    @testset "install_file: a refused FILE destination is moved aside, never deleted" begin
        # Windows refuses a rename over a file another process holds open
        # (a live inbox watcher holds comm-watch.sh). Modeled through the
        # `rename` seam: the first rename onto dst is refused, everything
        # else behaves. The old file must survive under an aside name (the
        # holder keeps it), dst must carry the new bytes, and a later
        # install prunes the aside copy.
        mktempdir() do dir
            src = joinpath(dir, "new.sh"); write(src, "NEW")
            dst = joinpath(dir, "held.sh"); write(dst, "OLD")
            refused = Ref(true)
            fake_rename(a, b) = begin
                if refused[] && b == dst && occursin(".tmp-", a)
                    refused[] = false
                    error("rename($a, $b): permission denied (EACCES)")
                end
                Base.Filesystem.rename(a, b)
            end
            ShipTools.install_file(src, dst; rename = fake_rename)
            @test read(dst, String) == "NEW"
            asides = filter(n -> startswith(n, "held.sh.stale-"), readdir(dir))
            @test length(asides) == 1
            @test read(joinpath(dir, asides[1]), String) == "OLD"
            @test !any(n -> occursin(".tmp-", n), readdir(dir))
            # Reaping moved INTO install_file's success path: the aside is
            # removed by the next successful replace of the same name, so the
            # real path is what gets asserted, not a helper called by hand.
            write(src, "NEWER")
            ShipTools.install_file(src, dst)
            @test read(dst, String) == "NEWER"
            @test !any(n -> occursin(".stale-", n), readdir(dir))

            # A refusal that persists even after the aside: the old file is
            # put back and the error names dst.
            write(dst, "OLD2")
            # Refuse every PUBLISH onto dst (a .tmp source); the restore of
            # the aside copy targets a now-free name and goes through, as it
            # would on Windows.
            always_refuse(a, b) = (b == dst && occursin(".tmp-", a)) ? error("still refused") : Base.Filesystem.rename(a, b)
            err = try
                ShipTools.install_file(src, dst; rename = always_refuse)
                nothing
            catch e
                e
            end
            @test err isa ErrorException
            @test occursin(dst, err.msg)
            @test read(dst, String) == "OLD2"
            @test !any(n -> occursin(".tmp-", n), readdir(dir))
        end
    end

    @testset "_check_installed" begin
        # Contract: returns a list of problem descriptions, never throws —
        # callers (_install_files) fold it into one combined report.
        mktempdir() do dir
            write(joinpath(dir, "present.sh"), "x")

            probs = ShipTools._check_installed(dir, ["present.sh", "missing.sh"])
            @test any(occursin("missing.sh", p) for p in probs)
            @test any(occursin(dir, p) for p in probs)

            probs2 = ShipTools._check_installed(dir, ["present.sh"]; executable = Returns(true))
            @test any(occursin("present.sh", p) for p in probs2)

            chmod(joinpath(dir, "present.sh"), 0o755)
            @test isempty(ShipTools._check_installed(dir, ["present.sh"]; executable = Returns(true)))
        end
    end

    @testset "_install_files: an identical destination is current even when it cannot be replaced" begin
        mktempdir() do tmp
            srcdir = joinpath(tmp, "src"); dstdir = joinpath(tmp, "dst")
            mkpath(srcdir); mkpath(dstdir)
            write(joinpath(srcdir, "same.sh"), "#!/bin/sh\necho same\n")
            write(joinpath(dstdir, "same.sh"), "#!/bin/sh\necho same\n")
            chmod(joinpath(dstdir, "same.sh"), 0o755)
            # Make the destination directory read-only so a replace would fail.
            chmod(dstdir, 0o555)
            try
                @test ShipTools._install_files(srcdir, dstdir, ["same.sh"]; executable = endswith(".sh")) === nothing
            finally
                chmod(dstdir, 0o755)
            end
        end
    end

    @testset "_install_files: continues past a locked destination, reports it, updates the rest" begin
        # The property the coordinator's field trace demanded: ONE destination
        # a live process still has open (comm-relay.sh, observed) must not
        # block updating the other N-1 files in the same directory, and the
        # final report must name exactly the one that stayed stale — never
        # silently, never by aborting everything else.
        mktempdir() do base
            srcdir = joinpath(base, "src")
            dstdir = joinpath(base, "dst")
            mkpath(srcdir)
            mkpath(dstdir)
            names = ["f$(lpad(i, 2, '0')).sh" for i in 1:12]
            stuck_name = names[11]
            for (i, name) in enumerate(names)
                write(joinpath(srcdir, name), "NEW-$i")
                if name == stuck_name
                    # A directory in its place: the rename step can never
                    # replace it, modeling a locked/in-use destination.
                    mkpath(joinpath(dstdir, name))
                    write(joinpath(dstdir, name, "marker.txt"), "keepme")
                else
                    write(joinpath(dstdir, name), "OLD-$i")  # a prior install
                end
            end

            err = try
                ShipTools._install_files(srcdir, dstdir, names; executable = endswith(".sh"))
                nothing
            catch e
                e
            end

            @test err isa ErrorException
            @test occursin(stuck_name, err.msg)

            # Every OTHER file updated — including ones AFTER the stuck one,
            # proving the loop did not stop early.
            for i in vcat(1:10, 12)
                @test read(joinpath(dstdir, names[i]), String) == "NEW-$i"
                @test Sys.isexecutable(joinpath(dstdir, names[i]))
            end
            # The stuck one is untouched — old "install" (the directory)
            # survives exactly as it was, not deleted, not half-replaced.
            @test isdir(joinpath(dstdir, stuck_name))
            @test read(joinpath(dstdir, stuck_name, "marker.txt"), String) == "keepme"
        end
    end

    @testset "_tmp_name: two live copies never share a staging path" begin
        # The load-bearing guard for the concurrency fix: a FIXED staging name
        # lets one run's copy interleave with another's rename and publish a
        # torn file under a name that then passes every existence check.
        dst = joinpath("/a", "b", "c.txt")
        a = ShipTools._tmp_name(dst)
        b = ShipTools._tmp_name(dst)
        @test a != b
        # A sibling, because the publish is a rename and a rename is atomic
        # only within one filesystem.
        @test dirname(a) == dirname(dst)
        @test a != dst * ".tmp"
        @test occursin(".tmp-", a)
    end

    @testset "install_file: a concurrent writer never publishes a torn file" begin
        mktempdir() do base
            dst = joinpath(base, "dst.bin")
            s1 = joinpath(base, "s1.bin"); s2 = joinpath(base, "s2.bin")
            write(s1, repeat("A", 1_000_000)); write(s2, repeat("B", 1_000_000))
            write(dst, "old")
            if Threads.nthreads() > 1
                for _ in 1:5
                    t1 = Threads.@spawn ShipTools.install_file(s1, dst)
                    t2 = Threads.@spawn ShipTools.install_file(s2, dst)
                    for t in (t1, t2)
                        try; fetch(t); catch; end
                    end
                    got = read(dst, String)
                    # Entirely one source or entirely the other — never mixed,
                    # never truncated, never missing.
                    @test got == read(s1, String) || got == read(s2, String)
                end
            else
                ShipTools.install_file(s1, dst)
                @test read(dst, String) == read(s1, String)
            end
        end
    end

    @testset "skills: an unremovable ORPHAN warns and does not kill the run" begin
        # The reproduced incident: a destination entry that cannot be
        # unlinked (NFS leaves a placeholder for a file a live process still
        # holds open) made the old rm-then-cp throw and killed every later
        # skill and stage. The directory itself was writable — only the
        # removal failed — so that is what this models: shipped files land at
        # the skill root, and the sweep meets an orphan it cannot remove.
        mktempdir() do base
            srcdir = joinpath(base, "src"); root = joinpath(base, "skills")
            for n in ("aaa-skill", "zzz-skill")
                mkpath(joinpath(srcdir, n))
                write(joinpath(srcdir, n, "SKILL.md"), "NEW-$n")
            end
            locked = joinpath(root, "aaa-skill", "retired")
            mkpath(locked)
            write(joinpath(locked, "held.md"), "OLD-held")
            write(joinpath(root, "aaa-skill", "SKILL.md"), "OLD")
            chmod(locked, 0o555)
            # Runtime probe, not a platform check: CI may run as a user whom
            # mode bits do not restrain.
            restrained = try
                rm(joinpath(locked, "held.md")); false
            catch
                true
            end
            try
                if restrained
                    ShipTools._install_skills(srcdir, root)
                    @test read(joinpath(root, "aaa-skill", "SKILL.md"), String) == "NEW-aaa-skill"
                    # A skill later in the walk order still installed.
                    @test read(joinpath(root, "zzz-skill", "SKILL.md"), String) == "NEW-zzz-skill"
                    # The orphan warned and stayed; it did not throw.
                    @test isfile(joinpath(locked, "held.md"))
                end
            finally
                chmod(locked, 0o755)
            end
        end
    end

    @testset "skills: one unreplaceable skill does not strand the skills after it" begin
        mktempdir() do base
            srcdir = joinpath(base, "src"); root = joinpath(base, "skills")
            names = ["s$(lpad(i, 2, '0'))" for i in 1:6]
            stuck = names[3]
            for n in names
                mkpath(joinpath(srcdir, n))
                write(joinpath(srcdir, n, "SKILL.md"), "NEW-$n")
            end
            mkpath(joinpath(root, stuck, "SKILL.md"))
            write(joinpath(root, stuck, "SKILL.md", "marker.txt"), "keepme")
            err = try
                ShipTools._install_skills(srcdir, root); nothing
            catch e
                e
            end
            @test err isa ErrorException
            @test occursin(stuck, err.msg)
            for n in names
                n == stuck && continue
                @test read(joinpath(root, n, "SKILL.md"), String) == "NEW-$n"
            end
            @test read(joinpath(root, stuck, "SKILL.md", "marker.txt"), String) == "keepme"
        end
    end

    @testset "skills: the orphan sweep removes retired files and spares markers" begin
        mktempdir() do base
            srcdir = joinpath(base, "src"); root = joinpath(base, "skills")
            mkpath(joinpath(srcdir, "sk", "references"))
            write(joinpath(srcdir, "sk", "SKILL.md"), "NEW")
            write(joinpath(srcdir, "sk", "references", "keep.md"), "KEEP")
            mkpath(joinpath(root, "sk", "references"))
            mkpath(joinpath(root, "sk", "gone"))
            write(joinpath(root, "sk", "references", "retired.md"), "OLD")
            write(joinpath(root, "sk", "gone", "old.md"), "OLD")
            # Derived foreign tag: guaranteed different without naming a host.
            foreign = "x" * ShipTools._host_tag()
            marker = joinpath(root, "sk", "SKILL.md.tmp-$foreign-424242-ab12")
            write(marker, "in-flight elsewhere")
            mkpath(joinpath(root, "my-user-skill"))
            write(joinpath(root, "my-user-skill", "SKILL.md"), "MINE")
            ShipTools._install_skills(srcdir, root)
            @test read(joinpath(root, "sk", "SKILL.md"), String) == "NEW"
            @test read(joinpath(root, "sk", "references", "keep.md"), String) == "KEEP"
            @test !isfile(joinpath(root, "sk", "references", "retired.md"))
            @test !isdir(joinpath(root, "sk", "gone"))
            @test isfile(marker)
            @test read(joinpath(root, "my-user-skill", "SKILL.md"), String) == "MINE"
        end
    end

    if Sys.isunix()
        @testset "install_file: marker reaping is owner-aware" begin
            mktempdir() do base
                dst = joinpath(base, "f.txt"); src = joinpath(base, "src.txt")
                write(dst, "OLD"); write(src, "NEW")
                # A certainly-dead pid: the child reports its own, then exits.
                proc = open(`sh -c "echo \$\$"`)
                deadpid = parse(Int, strip(read(proc, String)))
                wait(proc)
                tag = ShipTools._host_tag()
                mine_dead = joinpath(base, "f.txt.tmp-$tag-$deadpid-0001")
                foreign_live = joinpath(base, "f.txt.tmp-x$tag-$(getpid())-0002")
                aside = joinpath(base, "f.txt.stale-deadbeef")
                for f in (mine_dead, foreign_live, aside); write(f, "x"); end
                ShipTools.install_file(src, dst)
                @test read(dst, String) == "NEW"
                @test !isfile(mine_dead)      # this host, pid gone
                @test isfile(foreign_live)    # another host's pid means nothing here
                @test !isfile(aside)
            end
        end
    end

    @testset "update_comm reports an INCOMPLETE install honestly" begin
        mktempdir() do home
            adapters = joinpath(dirname(@__DIR__), "comm", "adapters", "claude")
            skill = first(sort([n for n in readdir(adapters)
                                if isfile(joinpath(adapters, n, "SKILL.md"))]))
            stuckdir = joinpath(home, ".claude", "skills", skill, "SKILL.md")
            mkpath(stuckdir)
            write(joinpath(stuckdir, "marker.txt"), "keepme")
            withenv("HOME" => home, "CLAUDE_CONFIG_DIR" => nothing, "CODEX_HOME" => nothing,
                    "SOT_COMM_HOME" => joinpath(home, ".sot-comm")) do
                err = try
                    ShipTools.update_comm(clis = [:claude]); nothing
                catch e
                    e
                end
                @test err isa ErrorException
                @test occursin("INCOMPLETE", err.msg)
                @test occursin(skill, err.msg)
                # No stamp on a partial run, and later stages were not stranded.
                @test !isfile(joinpath(home, ".sot-comm", "VERSION"))
                @test isfile(joinpath(home, ".sot-comm", "bin", "comm-relay.sh"))
            end
        end
    end

    @testset "env-dir resolution" begin
        # A set-but-empty override must read as unset: taking "" literally
        # yields a relative path that scatters the install into the CWD.
        withenv("CODEX_HOME" => "") do
            @test ShipTools.codex_home() == joinpath(homedir(), ".codex")
        end
        withenv("CODEX_HOME" => "/tmp/sot-test-codex-home") do
            @test ShipTools.codex_home() == "/tmp/sot-test-codex-home"
        end
        withenv("CODEX_HOME" => nothing) do
            @test ShipTools.codex_home() == joinpath(homedir(), ".codex")
        end
        withenv("SOT_COMM_HOME" => "") do
            @test ShipTools.comm_home() == joinpath(homedir(), ".sot-comm")
        end
        withenv("CLAUDE_CONFIG_DIR" => "") do
            @test ShipTools.claude_home() == joinpath(homedir(), ".claude")
        end
        withenv("CLAUDE_CONFIG_DIR" => "/tmp/sot-test-claude-home") do
            @test ShipTools.claude_home() == "/tmp/sot-test-claude-home"
        end
        withenv("CLAUDE_CONFIG_DIR" => nothing) do
            @test ShipTools.claude_home() == joinpath(homedir(), ".claude")
        end
    end

    @testset "codex home profile export parsing" begin
        # _parse_codex_home_export is pure and file-free by design — never
        # sources or runs a profile — so these exercise it directly on
        # synthetic profile text; no real dotfile is ever touched.
        user, home = "devuser", "/home/devuser"

        # A plain path needs no expansion.
        v, raw = ShipTools._parse_codex_home_export(
            "export CODEX_HOME=/opt/codex-home\n"; user, home)
        @test v == "/opt/codex-home"
        @test raw == "/opt/codex-home"

        # $HOME expands.
        v, raw = ShipTools._parse_codex_home_export(
            "export CODEX_HOME=\$HOME/.codex-work\n"; user, home)
        @test v == "/home/devuser/.codex-work"
        @test raw == "\$HOME/.codex-work"

        # ${USER:-$(id -un)} expands to the current user.
        v, raw = ShipTools._parse_codex_home_export(
            "export CODEX_HOME=/srv/agents/\${USER:-\$(id -un)}/codex\n"; user, home)
        @test v == "/srv/agents/devuser/codex"

        # The `NAME=value; export NAME` two-step form.
        v, raw = ShipTools._parse_codex_home_export(
            "CODEX_HOME=/opt/other; export CODEX_HOME\n"; user, home)
        @test v == "/opt/other"

        # A commented-out line must be ignored entirely.
        v, raw = ShipTools._parse_codex_home_export(
            "# export CODEX_HOME=/should/not/count\n"; user, home)
        @test v === nothing
        @test raw === nothing

        # No assignment anywhere in the file.
        v, raw = ShipTools._parse_codex_home_export(
            "export PATH=\$PATH:/usr/local/bin\nalias ll='ls -la'\n"; user, home)
        @test v === nothing
        @test raw === nothing

        # A value this installer doesn't know how to expand is reported,
        # not guessed at — the raw text survives so a warning can quote it.
        v, raw = ShipTools._parse_codex_home_export(
            "export CODEX_HOME=\$SOME_OTHER_VAR/codex\n"; user, home)
        @test v === nothing
        @test raw == "\$SOME_OTHER_VAR/codex"
    end

    @testset "accounts: the installer never writes under .claude-auth" begin
        # Owner ruling: a named account shares the default `~/.claude`
        # folder by SYMLINK, created by the daemon at spawn
        # (`rust/backend/src/accounts.rs::ensure_account_links`), not by
        # this installer. The guard: even with an existing account
        # subdirectory sitting right there, a full install must leave it
        # untouched.
        mktempdir() do home
            acct_dir = joinpath(home, ".claude-auth", "acct")
            mkpath(acct_dir)
            withenv("HOME" => home, "CLAUDE_CONFIG_DIR" => nothing, "CODEX_HOME" => nothing,
                    "SOT_COMM_HOME" => joinpath(home, ".sot-comm")) do
                ShipTools.update_comm(clis = [:claude])
                @test isempty(readdir(acct_dir))
                @test readdir(joinpath(home, ".claude-auth")) == ["acct"]
            end
        end
    end

    @testset "installer prunes the retired session-start aliases and ccbe" begin
        # Both adapters route through _install_skills: the Codex-only path
        # that never carried resource files must not silently return.
        @test !isdefined(ShipTools, :_install_claude_skills)
        # COMM_DEPRECATED_SKILLS / COMM_DEPRECATED_LAUNCHERS: exact names
        # only. A skill or launcher the repo stopped shipping (a prior
        # install left it on disk) must be removed from BOTH the Claude and
        # Codex skills dirs, and from the shared launcher dir, on every
        # install — while an unrelated user skill/launcher in the same
        # directories survives untouched.
        mktempdir() do home
            claude_skills = joinpath(home, ".claude", "skills")
            codex_skills = joinpath(home, ".codex", "skills")
            bindir = joinpath(home, ".local", "bin")
            for skills in (claude_skills, codex_skills)
                for name in ("sot-be-session-start", "sot-fe-session-start", "my-user-skill")
                    mkpath(joinpath(skills, name))
                    write(joinpath(skills, name, "SKILL.md"), "---\nname: $name\n---\nstub\n")
                end
            end
            mkpath(bindir)
            write(joinpath(bindir, "ccbe"), "#!/bin/sh\necho stale\n")
            write(joinpath(bindir, "my-launcher"), "#!/bin/sh\necho keepme\n")

            withenv("HOME" => home, "CLAUDE_CONFIG_DIR" => nothing, "CODEX_HOME" => nothing,
                    "SOT_COMM_HOME" => joinpath(home, ".sot-comm")) do
                ShipTools.update_comm(clis = [:claude, :codex])
            end

            for skills in (claude_skills, codex_skills)
                @test !isdir(joinpath(skills, "sot-be-session-start"))
                @test !isdir(joinpath(skills, "sot-fe-session-start"))
                @test isdir(joinpath(skills, "my-user-skill"))
            end
            @test !isfile(joinpath(bindir, "ccbe"))
            @test isfile(joinpath(bindir, "my-launcher"))
        end
    end

    @testset "project-local skills match their shipped copies" begin
        # A fresh checkout runs /sot-setup (and the skills it calls) from
        # .claude/skills before anything is installed; the installer ships
        # comm/adapters/claude. A skill held in both must be byte-identical.
        root = dirname(@__DIR__)
        localdir = joinpath(root, ".claude", "skills")
        shipped = joinpath(root, "comm", "adapters", "claude")
        relfiles(d) = sort([relpath(joinpath(r, f), d) for (r, _, fs) in walkdir(d) for f in fs])
        for name in readdir(localdir)
            isdir(joinpath(shipped, name)) || continue
            a, b = joinpath(localdir, name), joinpath(shipped, name)
            @test relfiles(a) == relfiles(b)
            for rel in relfiles(a)
                @test read(joinpath(a, rel)) == read(joinpath(b, rel))
            end
        end
    end
end
