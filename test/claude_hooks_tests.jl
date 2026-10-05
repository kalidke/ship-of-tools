# Tests for src/claude_hooks.jl: Claude settings targets and hook merging (calls jq).

@testset "accounts: an account with no settings.json is left for the daemon's link" begin
    # A named account shares the default `~/.claude` folder by SYMLINK,
    # created by the daemon at spawn (`rust/backend/src/agents/accounts.rs::
    # ensure_account_links`), which leaves any existing entry alone. So
    # the installer must never CREATE a settings.json in an account
    # folder: a real file there would shadow the shared one for good.
    # An empty account folder stays empty through a full install.
    mktempdir() do home
        acct_dir = joinpath(home, ".claude-auth", "acct")
        mkpath(acct_dir)
        in_home(home) do
            ShipTools.update_comm(clis = [:claude])
            @test isempty(readdir(acct_dir))
            @test readdir(joinpath(home, ".claude-auth")) == ["acct"]
        end
    end
end

@testset "_claude_settings_targets: every account, each real file once" begin
    mktempdir() do home
        default = joinpath(home, ".claude", "settings.json")
        mkpath(dirname(default)); write(default, "{}")
        auth = joinpath(home, ".claude-auth")
        a = joinpath(auth, "a", "settings.json")
        mkpath(dirname(a)); write(a, "{}")                       # its own real file
        b = joinpath(auth, "b", "settings.json")
        mkpath(dirname(b)); symlink(default, b)                  # the daemon's link
        mkpath(joinpath(auth, "c"))                              # no settings.json yet
        d = joinpath(auth, "d", "settings.json")
        mkpath(dirname(d)); symlink(joinpath(home, "missing.json"), d)  # dangling
        bad = joinpath(auth, "Bad.Name", "settings.json")
        mkpath(dirname(bad)); write(bad, "{}")                   # not an account name
        write(joinpath(auth, "notadir"), "")                     # a stray file

        @test_logs (:info, r"no settings.json in this Claude account folder") match_mode = :any ShipTools._claude_settings_targets(home, joinpath(home, ".claude"))
        @test ShipTools._claude_settings_targets(home, joinpath(home, ".claude")) ==
              [realpath(default), realpath(a)]
        # An install run from inside an account folder with no settings.json
        # never creates one there (ruling 1); the default takes the hooks.
        own = joinpath(auth, "c")
        @test ShipTools._claude_settings_targets(home, own) ==
              [realpath(default), realpath(a)]
        # Nothing was created through the dangling link.
        @test islink(d) && !ispath(joinpath(home, "missing.json"))

        # The daemon's name rule, character for character.
        for ok in ("team", "a_b-2", "0x")
            @test ShipTools._is_account_name(ok)
        end
        for no in ("Team", "..", "-x", "", "team\n", "a/b", "a.b")
            @test !ShipTools._is_account_name(no)
        end
    end
end

@testset "_install_claude_hooks: comm hooks land in every account's real settings.json" begin
    # Owner ruling: the comm hooks go into EVERY Claude account's
    # settings.json, links resolved, each real file written once, the
    # user's own hooks and settings kept, a re-run byte-identical. The
    # default here is itself a link to a file outside every claude dir:
    # a copy-then-rename onto the LINK path would replace the link with
    # a plain copy, so this also proves a link stays a link.
    jqprog = "(.hooks[\$e] // [])[] | (.hooks // [])[] | .command"
    cmds(f, ev) = split(readchomp(`jq -r --arg e $ev $jqprog $f`), r"\r?\n"; keepempty = false)
    mktempdir() do home
        shared = joinpath(home, "dotfiles", "claude-settings.json")
        mkpath(dirname(shared))
        write(shared, """{"model":"shared","hooks":{"Stop":[{"hooks":[{"type":"command","command":"/usr/local/bin/user-shared.sh"}]}]}}""")
        default = joinpath(home, ".claude", "settings.json")
        mkpath(dirname(default)); symlink(shared, default)
        auth = joinpath(home, ".claude-auth")
        a = joinpath(auth, "a", "settings.json")
        mkpath(dirname(a))
        write(a, """{"model":"own","hooks":{"Stop":[{"hooks":[{"type":"command","command":"/usr/local/bin/user-a.sh"}]}],"Notification":[{"hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-status-blocked.sh"}]}]}}""")
        b = joinpath(auth, "b", "settings.json")
        mkpath(dirname(b)); symlink(default, b)
        c = joinpath(auth, "c"); mkpath(c)
        in_home(home) do
            ShipTools._install_claude_hooks(ShipTools.claude_home())

            # Each real file holds each comm hook exactly once.
            for f in (shared, a), (ev, script, _) in ShipTools._COMM_STATE_HOOKS
                @test count(==(ShipTools._hook_command(script)), cmds(f, ev)) == 1
            end
            # The user's own hooks and settings survive; the retired comm
            # hook in the account's own file is gone.
            @test "/usr/local/bin/user-shared.sh" in cmds(shared, "Stop")
            @test "/usr/local/bin/user-a.sh" in cmds(a, "Stop")
            @test readchomp(`jq -r .model $shared`) == "shared"
            @test readchomp(`jq -r .model $a`) == "own"
            @test isempty(cmds(a, "Notification"))
            # Links stay links, to the same targets; the empty account stays empty.
            @test islink(default) && readlink(default) == shared
            @test islink(b) && readlink(b) == default
            @test isempty(readdir(c))

            # A re-run changes nothing.
            before = Dict(f => read(f) for f in (shared, a))
            ShipTools._install_claude_hooks(ShipTools.claude_home())
            for f in (shared, a)
                @test read(f) == before[f]
            end
            @test islink(default) && readlink(default) == shared
            @test islink(b) && readlink(b) == default
        end
    end
end

@testset "_install_claude_hooks: a linked settings.json stays a link" begin
    jqprog = "(.hooks[\$e] // [])[] | (.hooks // [])[] | .command"
    cmds(f, ev) = split(readchomp(`jq -r --arg e $ev $jqprog $f`), r"\r?\n"; keepempty = false)
    mktempdir() do home
        target = joinpath(home, "elsewhere", "settings.json")
        mkpath(dirname(target))
        write(target, """{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/usr/local/bin/user.sh"}]}]}}""")
        link = joinpath(home, ".claude", "settings.json")
        mkpath(dirname(link)); symlink(target, link)
        in_home(home) do
            ShipTools._install_claude_hooks(ShipTools.claude_home())
            @test islink(link) && readlink(link) == target
            for (ev, script, _) in ShipTools._COMM_STATE_HOOKS
                @test count(==(ShipTools._hook_command(script)), cmds(target, ev)) == 1
            end
            @test "/usr/local/bin/user.sh" in cmds(target, "Stop")
        end
    end
end

@testset "_install_claude_hooks: never creates a settings.json in an account folder" begin
    jqprog = "(.hooks[\$e] // [])[] | (.hooks // [])[] | .command"
    cmds(f, ev) = split(readchomp(`jq -r --arg e $ev $jqprog $f`), r"\r?\n"; keepempty = false)
    mktempdir() do home
        mkpath(joinpath(home, ".claude"))
        c = joinpath(home, ".claude-auth", "c"); mkpath(c)
        in_home(home) do
            ShipTools._install_claude_hooks(c)
            @test isempty(readdir(c))
            f = joinpath(home, ".claude", "settings.json")
            @test isfile(f)
            for (ev, script, _) in ShipTools._COMM_STATE_HOOKS
                @test count(==(ShipTools._hook_command(script)), cmds(f, ev)) == 1
            end
        end
    end
end

@testset "_claude_settings_targets: an unreadable account folder is skipped, not fatal" begin
    mktempdir() do home
        default = joinpath(home, ".claude", "settings.json")
        mkpath(dirname(default)); write(default, "{}")
        a = joinpath(home, ".claude-auth", "a", "settings.json")
        mkpath(dirname(a)); write(a, "{}")
        locked = joinpath(home, ".claude-auth", "locked")
        mkpath(locked); write(joinpath(locked, "settings.json"), "{}")
        chmod(locked, 0o000)
        try
            restrained = try isfile(joinpath(locked, "settings.json")); false catch; true end
            if !restrained
                @test_skip false
            else
                @test ShipTools._claude_settings_targets(home, joinpath(home, ".claude")) ==
                      [realpath(default), realpath(a)]
            end
        finally
            chmod(locked, 0o755)
        end
    end
end

@testset "_install_claude_hooks: a rewrite keeps the file's mode and a re-run does not rewrite" begin
    if Sys.iswindows()
        @test_skip false
    else
        jqprog = "(.hooks[\$e] // [])[] | (.hooks // [])[] | .command"
        cmds(f, ev) = split(readchomp(`jq -r --arg e $ev $jqprog $f`), r"\r?\n"; keepempty = false)
        mktempdir() do home
            f = joinpath(home, ".claude", "settings.json")
            mkpath(dirname(f)); write(f, "{}")
            chmod(f, 0o604)  # a mode no umask produces
            in_home(home) do
                ShipTools._install_claude_hooks(joinpath(home, ".claude"))
                @test filemode(f) & 0o777 == 0o604
                for (ev, script, _) in ShipTools._COMM_STATE_HOOKS
                    @test count(==(ShipTools._hook_command(script)), cmds(f, ev)) == 1
                end
                ino1 = stat(f).inode
                ShipTools._install_claude_hooks(joinpath(home, ".claude"))
                @test stat(f).inode == ino1
                @test filemode(f) & 0o777 == 0o604
            end
        end
    end
end

@testset "_settings_tmp: the staging file has the settings file's mode before it holds anything" begin
    if Sys.iswindows()
        @test_skip false
    else
        mktempdir() do dir
            f = joinpath(dir, "settings.json")
            write(f, "{}")
            chmod(f, 0o604)
            t = ShipTools._settings_tmp(f)
            @test isfile(t)
            @test filesize(t) == 0
            @test filemode(t) & 0o777 == 0o604
            rm(t)
        end
    end
end

@testset "_install_claude_hooks: nothing is created in .claude-auth through a relative, linked or bare config dir" begin
    jqprog = "(.hooks[\$e] // [])[] | (.hooks // [])[] | .command"
    cmds(f, ev) = split(readchomp(`jq -r --arg e $ev $jqprog $f`), r"\r?\n"; keepempty = false)
    mktempdir() do home
        default = joinpath(home, ".claude", "settings.json")
        mkpath(dirname(default)); write(default, "{}")
        auth = joinpath(home, ".claude-auth")
        c = joinpath(auth, "c"); mkpath(c)
        symlink(c, joinpath(home, "alias"))
        in_home(home) do
            cd(home) do
                ShipTools._install_claude_hooks(joinpath(".claude-auth", "c"))
            end
            ShipTools._install_claude_hooks(joinpath(home, "alias"))
            ShipTools._install_claude_hooks(auth)
            @test isempty(readdir(c))
            @test !ispath(joinpath(auth, "settings.json"))
            for (ev, script, _) in ShipTools._COMM_STATE_HOOKS
                @test count(==(ShipTools._hook_command(script)), cmds(default, ev)) == 1
            end
        end
    end
end

@testset "_claude_settings_targets: an account link into an unsearchable dir is skipped, not fatal" begin
    mktempdir() do home
        default = joinpath(home, ".claude", "settings.json")
        mkpath(dirname(default)); write(default, "{}")
        a = joinpath(home, ".claude-auth", "a", "settings.json")
        mkpath(dirname(a)); write(a, "{}")
        locked = joinpath(home, "locked")
        mkpath(joinpath(locked, "target"))
        symlink(joinpath(locked, "target"), joinpath(home, ".claude-auth", "x"))
        chmod(locked, 0o000)
        try
            restrained = try isfile(joinpath(locked, "probe")); false catch; true end
            if !restrained
                @test_skip false
            else
                @test ShipTools._claude_settings_targets(home, joinpath(home, ".claude")) ==
                      [realpath(default), realpath(a)]
            end
        finally
            chmod(locked, 0o755)
        end
    end
end

@testset "_install_claude_hooks: a merge that throws fails the install, after the other files are done" begin
    jqprog = "(.hooks[\$e] // [])[] | (.hooks // [])[] | .command"
    cmds(f, ev) = split(readchomp(`jq -r --arg e $ev $jqprog $f`), r"\r?\n"; keepempty = false)
    mktempdir() do home
        a = joinpath(home, ".claude-auth", "a", "settings.json")
        mkpath(dirname(a)); write(a, "{}")
        ro = joinpath(home, "ro"); mkpath(ro)
        chmod(ro, 0o555)
        try
            restrained = try touch(joinpath(ro, "probe")); rm(joinpath(ro, "probe")); false catch; true end
            if !restrained
                @test_skip false
            else
                in_home(home) do
                    @test_throws ErrorException ShipTools._install_claude_hooks(ro)
                    for (ev, script, _) in ShipTools._COMM_STATE_HOOKS
                        @test count(==(ShipTools._hook_command(script)), cmds(a, ev)) == 1
                    end
                    @test !ispath(joinpath(ro, "settings.json"))
                end
            end
        finally
            chmod(ro, 0o755)
        end
    end
end

@testset "_install_claude_hooks: an account folder that is a link counts as inside .claude-auth" begin
    jqprog = "(.hooks[\$e] // [])[] | (.hooks // [])[] | .command"
    cmds(f, ev) = split(readchomp(`jq -r --arg e $ev $jqprog $f`), r"\r?\n"; keepempty = false)
    mktempdir() do home
        default = joinpath(home, ".claude", "settings.json")
        mkpath(dirname(default)); write(default, "{}")
        elsewhere = joinpath(home, "disk", "team")
        mkpath(elsewhere)
        mkpath(joinpath(home, ".claude-auth"))
        team = joinpath(home, ".claude-auth", "team")
        symlink(elsewhere, team)
        in_home(home) do
            ShipTools._install_claude_hooks(team)
            @test isempty(readdir(elsewhere))
            for (ev, script, _) in ShipTools._COMM_STATE_HOOKS
                @test count(==(ShipTools._hook_command(script)), cmds(default, ev)) == 1
            end
        end
    end
end

@testset "install: the closing summary names every settings file left without the hooks" begin
    mktempdir() do home
        default = joinpath(home, ".claude", "settings.json")
        mkpath(dirname(default)); write(default, "{}")
        lockd = joinpath(home, ".claude-auth", "lockd", "settings.json")
        mkpath(dirname(lockd))
        write(lockd, "{}")
        chmod(lockd, 0o444)
        empty = joinpath(home, ".claude-auth", "empty")
        mkpath(empty)
        try
            restrained = try open(lockd, "a") do _ end; false catch; true end
            if !restrained
                @test_skip false
            else
                in_home(home) do
                    logger = Test.TestLogger(min_level = Base.CoreLogging.Warn)
                    Base.CoreLogging.with_logger(logger) do
                        ShipTools.update_comm(clis = [:claude])
                    end
                    summaries = filter(r -> occursin("The comm hooks are NOT in these Claude settings", string(r.message)), logger.logs)
                    @test length(summaries) == 1
                    msg = string(only(summaries).message)
                    # The summary names real paths: on Windows mktempdir() is an 8.3 short path and realpath the long one.
                    @test occursin(realpath(lockd), msg)
                    @test occursin(realpath(empty), msg)
                    @test !occursin(realpath(default), msg)
                    @test read(lockd, String) == "{}"   # left exactly as it was
                end
            end
        finally
            chmod(lockd, 0o644)
        end
    end
end

@testset "_remove_stale_comm_hooks!: retired reminder hooks go, others stay" begin
    # The post-compact and post-clear reminder scripts are deleted, so an
    # install must also drop their settings entries, or settings.json keeps
    # two hooks pointing at files that no longer exist.
    mktempdir() do dir
        settings = joinpath(dir, "settings.json")
        open(settings, "w") do io
            write(io, """{"hooks":{
              "SessionStart":[
                {"matcher":"compact","hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-postcompact-reminder.sh"}]},
                {"matcher":"clear","hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-postclear-reminder.sh"}]},
                {"matcher":"startup","hooks":[{"type":"command","command":"/usr/local/bin/mine.sh"}]}],
              "Stop":[{"hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-status-idle.sh"}]}],
              "Notification":[{"hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-status-blocked.sh"}]}],
              "PostToolUse":[{"hooks":[{"type":"command","command":"/usr/local/bin/other.sh"}]}]}}""")
        end
        ShipTools._remove_stale_comm_hooks!(settings)
        txt = read(settings, String)
        @test !occursin("comm-postcompact-reminder", txt)
        @test !occursin("comm-postclear-reminder", txt)
        @test occursin("comm-status-idle", txt)
        @test occursin("/usr/local/bin/mine.sh", txt)
        @test occursin("/usr/local/bin/other.sh", txt)
        @test occursin("\"Stop\"", txt)
        @test !occursin("\"Notification\"", txt)
    end
    mktempdir() do dir
        settings = joinpath(dir, "settings.json")
        write(settings, """{"hooks":{"Notification":[{"hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-status-blocked.sh"},{"type":"command","command":"/usr/local/bin/mine.sh"}]}]}}""")
        ShipTools._remove_stale_comm_hooks!(settings)
        got = split(readchomp(`jq -r '.hooks.Notification[].hooks[].command' $settings`), r"\r?\n"; keepempty = false)
        @test got == ["/usr/local/bin/mine.sh"]
    end
    mktempdir() do dir
        settings = joinpath(dir, "settings.json")
        write(settings, """{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/usr/local/bin/comm-status-report.sh"}]}]}}""")
        ShipTools._remove_stale_comm_hooks!(settings)
        got = split(readchomp(`jq -r '.hooks.Stop[].hooks[].command' $settings`), r"\r?\n"; keepempty = false)
        @test got == ["/usr/local/bin/comm-status-report.sh"]
    end
end
