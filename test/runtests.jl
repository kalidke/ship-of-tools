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
            @test !isfile(dst * ".tmp")

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
    end
end
