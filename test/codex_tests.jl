# Tests for src/codex.jl: the Codex hooks payload, marketplace registry and home profile export parsing.

@testset "codex hooks payload" begin
    # This file has died silently twice — an unrecognized top-level key
    # (codex rejects the WHOLE file) and a wrong event-key case (the key is
    # dropped). Neither reports an error anywhere: the only symptom is that
    # every codex session stops reporting its work-state, which reads as a
    # Ship of Tools bug rather than a parse failure. These checks are the
    # cheap standing guard; see comm/adapters/codex/hooks.README.md.
    hooks_path = ShipTools.CODEX_HOOKS_JSON_SRC
    @test isfile(hooks_path)
    txt = read(hooks_path, String)

    # (1) Real JSON. Also covers the scanner's one blind spot: on malformed
    # input a stray key can sit at depth <= 0 and escape detection.
    @test_nowarn ShipTools._json_toplevel_keys(txt)
    @test success(pipeline(`jq empty $hooks_path`))

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
    # at a file that never arrives. The bin gets every listed folder's files
    # whatever `clis`, so `install_comm(clis = [:codex])` on its own deploys
    # the scripts the payload references too.
    sources = ShipTools._comm_bin_shipped()
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
    for txt in payloads
        @test success(pipeline(pipeline(IOBuffer(txt), `jq empty`)))
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

# Include resource bytes and relative names, so a missing, stale or extra resource fails.
function plugin_tree(root)
    Dict(relpath(joinpath(dir, f), root) => read(joinpath(dir, f))
         for (dir, _, files) in walkdir(root) for f in files)
end

@testset "Codex row plugin install, refresh and recovery" begin
    mktempdir() do home
        tools = mkpath(joinpath(home, "tools"))
        cp(realpath(something(Sys.which("jq"))), joinpath(tools, Sys.iswindows() ? "jq.exe" : "jq"))
        in_home(home, "PATH" => tools) do
            skills = joinpath(ShipTools.REPO_ROOT, "agents", "claude")
            names = readlines(joinpath(dirname(skills), "codex", "shared-skills.txt"))
            marketplace = joinpath(home, ".agents", "plugins")
            cfg = joinpath(ShipTools.codex_home(), "config.toml")
            ShipTools.update_comm(clis = [:codex])
            # Resolve the plugin id from the installed launcher's actual override. If either end drifts the cache lookup fails.
            launcher = read(joinpath(home, ".local", "bin", "ccx"), String)
            id = match(r"-c 'plugins\.([^@']+)@([^']+)\.enabled=true'", launcher)
            @test id !== nothing
            name, market = id.captures
            cache = joinpath(ShipTools.codex_home(), "plugins", "cache", market, name)
            @test !ispath(joinpath(marketplace, name))
            @test !occursin(name, read(joinpath(marketplace, "marketplace.json"), String))
            @test !ispath(cfg)
            @test readdir(joinpath(ShipTools.codex_home(), "skills")) == ["sot-chat-post", "sot-comm", "sot-session-start"]
            if Sys.iswindows()
                @test !ispath(cache) && !ispath(joinpath(dirname(cache), ".sot-row.install"))
                return
            end
            root = joinpath(cache, only(readdir(cache)))
            @test readdir(root) == [".codex-plugin", "skills"]
            @test readdir(joinpath(root, "skills")) == sort(names)
            manifest = joinpath(root, ".codex-plugin", "plugin.json")
            @test success(pipeline(`jq -e --arg name $name '.name == $name and .skills == "./skills/" and (has("hooks") | not)' $manifest`; stdout = devnull))
            for n in names
                @test plugin_tree(joinpath(root, "skills", n)) == plugin_tree(joinpath(skills, n))
            end
            before = plugin_tree(cache)
            write(joinpath(root, "obsolete"), "older release")
            ShipTools.update_comm(clis = [:codex])
            @test before == plugin_tree(cache)
            for personal in ("# unrelated plugin\n[plugins.\"other@local\"]\nenabled = true\n",
                             "[plugins.\"sot-row@sot-local\"]\nenabled = false # my choice\n",
                             "plugins = { \"sot-row@sot-local\" = { enabled = true } }\n")
                write(cfg, personal)
                ShipTools.update_comm(clis = [:codex])
                @test read(cfg, String) == personal
            end
        end
    end
end

@testset "Row cache publication" begin
    rules = Sys.iswindows() ? (:windows,) : (:windows, :interrupted_publish, :dead_new, :dead_previous,
        :move_in, :personal, :replace_cache, :missing_skill, :incomplete, :lock_folder, :lock_file_link,
        :lock_dangling_link, :dead_lock, :live_lock)
    for rule in rules
        @testset "$rule" begin
            mktempdir() do home
                tools = mkpath(joinpath(home, "tools"))
                cp(realpath(something(Sys.which("jq"))), joinpath(tools, Sys.iswindows() ? "jq.exe" : "jq"))
                in_home(home, "PATH" => tools) do
                    cache = joinpath(ShipTools.codex_home(), "plugins", "cache", "sot-local", "sot-row")
                    if rule === :windows
                        ShipTools._publish_row_tree(cache; windows = true) do stage
                            error("Windows tried to build the row plugin in $stage")
                        end
                        @test !ispath(dirname(cache))
                        return
                    end
                    scratch = joinpath(mkpath(dirname(cache)), ".sot-row.install")
                    lock = joinpath(scratch, "lock")
                    stamp = joinpath(ShipTools.comm_home(), "VERSION")
                    update() = ShipTools.update_comm(clis = [:codex])
                    if rule === :interrupted_publish
                        ShipTools._publish_row_tree(cache) do stage
                            write(joinpath(mkpath(stage), "kept"), "old")
                        end
                        mv(cache, joinpath(scratch, "previous"))
                        @test_throws "stop build" ShipTools._publish_row_tree(cache) do stage
                            @test read(joinpath(cache, "kept"), String) == "old"
                            error("stop build")
                        end
                        @test isfile(joinpath(cache, "kept"))
                    elseif rule === :dead_new
                        ShipTools._publish_row_tree(mkpath, cache)
                        skills = joinpath(ShipTools.ROW_PLUGIN_VERSION, "skills")
                        dropped = mkpath(joinpath(scratch, "new", skills, "dropped-skill"))
                        write(joinpath(dropped, "SKILL.md"), "# Dropped skill")
                        update()
                        @test readdir(joinpath(cache, skills)) == sort(readlines(ShipTools.CODEX_ROW_SKILL_LIST))
                    elseif rule === :dead_previous
                        ShipTools._publish_row_tree(mkpath, cache)
                        previous = mkpath(joinpath(scratch, "previous"))
                        write(joinpath(previous, "old"), "previous release")
                        update()
                        @test isfile(stamp) && isdir(cache) && !ispath(previous)
                    elseif rule === :move_in
                        write(joinpath(mkpath(cache), "kept"), "old")
                        @test_throws Exception ShipTools._publish_row_tree(identity, cache)
                        @test read(joinpath(cache, "kept"), String) == "old"
                    elseif rule === :personal
                        refused() = @test_throws "does not belong to the installer: $scratch" ShipTools._publish_row_tree(mkpath, cache)
                        write(scratch, "personal")
                        refused()
                        @test read(scratch, String) == "personal"
                        rm(scratch)
                        mkdir(scratch)
                        refused()
                        @test isempty(readdir(scratch))
                        write(joinpath(scratch, "new"), "personal")
                        refused()
                        @test read(joinpath(scratch, "new"), String) == "personal"
                        write(joinpath(scratch, "mine"), "personal")
                        refused()
                        @test read(joinpath(scratch, "mine"), String) == "personal"
                        if Sys.isunix()
                            rm(scratch; recursive = true)
                            target = mkpath(joinpath(home, "elsewhere"))
                            symlink(target, scratch)
                            refused()
                            rm(target)
                            refused()
                            @test readlink(scratch) == target
                        end
                    elseif rule === :replace_cache
                        for kind in (Sys.isunix() ? (:file, :folder, :link, :dangling) : (:file, :folder))
                            target = joinpath(home, "target")
                            kind === :file && write(cache, "personal")
                            kind === :folder && write(joinpath(mkpath(cache), "mine"), "personal")
                            kind === :link && mkpath(target)
                            kind in (:link, :dangling) && symlink(target, cache)
                            ShipTools._publish_row_tree(cache) do stage
                                write(joinpath(mkpath(stage), "new"), "release")
                            end
                            @test !islink(cache) && readdir(cache) == ["new"]
                            rm(cache; recursive = true)
                            if kind === :link
                                @test isdir(target)
                                rm(target)
                            end
                        end
                    elseif rule === :missing_skill
                        @test_throws "missing a listed skill" ShipTools._install_row_plugin(ShipTools.codex_home(), mkpath(joinpath(home, "skills")))
                        @test !ispath(cache)
                    elseif rule === :incomplete
                        write(scratch, "personal")
                        @test_throws "install INCOMPLETE" update()
                        @test !ispath(stamp)
                        @test isfile(joinpath(home, ".agents", "plugins", "marketplace.json"))
                    elseif rule in (:lock_folder, :lock_file_link, :lock_dangling_link)
                        touch(joinpath(mkpath(scratch), "sot-row-stage"))
                        target = joinpath(scratch, "target")
                        if rule === :lock_folder
                            write(joinpath(mkdir(lock), "keep"), "personal")
                        else
                            rule === :lock_file_link && write(target, "personal")
                            symlink(basename(target), lock)
                        end
                        entries = readdir(scratch)
                        @test_throws ["install INCOMPLETE", "row plugin lock does not belong to the installer: $lock"] update()
                        @test readdir(scratch) == entries
                        @test !ispath(cache) && !ispath(stamp)
                        if rule === :lock_folder
                            @test isdir(lstat(lock)) && readdir(lock) == ["keep"] && read(joinpath(lock, "keep"), String) == "personal"
                        else
                            @test islink(lock) && readlink(lock) == basename(target)
                            @test rule === :lock_file_link ? read(target, String) == "personal" : !ispath(lstat(target))
                        end
                    elseif rule === :dead_lock
                        mkpath(scratch)
                        touch(joinpath(scratch, "sot-row-stage"))
                        write(lock, "$(typemax(Int32)) $(gethostname())")
                        inode = stat(lock)
                        @test (@elapsed update()) < 30
                        @test isfile(stamp) && isdir(cache) && Base.Filesystem.samefile(inode, stat(lock))
                        holder = ShipTools._row_lock(lock)
                        @test holder !== nothing
                        isnothing(holder) || close(holder)
                    else
                        mkpath(scratch)
                        touch(joinpath(scratch, "sot-row-stage"))
                        holder = ShipTools._row_lock(lock)
                        try
                            @test_logs (:info, r"Skipping row plugin") match_mode=:any update()
                            @test isfile(stamp) && !ispath(cache)
                            @test isfile(joinpath(home, ".agents", "plugins", "marketplace.json"))
                        finally
                            close(holder)
                        end
                    end
                end
            end
        end
    end
end

@testset "Claude-only install keeps row skills in its own layer" begin
    mktempdir() do home
        in_home(home) do
            ShipTools.update_comm(clis = [:claude])
            @test !ispath(joinpath(home, ".agents", "plugins", "sot-row"))
            @test !ispath(joinpath(ShipTools.codex_home(), "plugins"))
            expected = Dict(n => plugin_tree(joinpath(src, n)) for src in ShipTools.CLAUDE_SKILL_SRCS
                for n in readdir(src) if isfile(joinpath(src, n, "SKILL.md")) && !(n in ShipTools.CLAUDE_GLOBAL_SKILLS))
            layer = ShipTools.claude_layer_skills()
            @test Dict(n => plugin_tree(joinpath(layer, n)) for n in readdir(layer)) == expected
        end
    end
end
