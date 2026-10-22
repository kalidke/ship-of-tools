# Tests for src/claude_hooks.jl: Claude settings targets and the retirement of the comm hooks (calls jq).

# jq's raw output, one string per line. On Windows jq writes CRLF line endings (its text mode), so there the one CR
# before each LF is the line ending; any other CR is data and stays.
jq_lines(cmd::Cmd) = [Sys.iswindows() ? chopsuffix(l, "\r") : l for l in split(read(cmd, String), '\n'; keepempty = false)]
# Every hook command of settings file `f`, over all events.
every_cmd(f) = jq_lines(`jq -r "[(.hooks // {})[] | .[] | (.hooks // [])[] | .command] | .[]" $f`)

# The hooks of an earlier install, with a person's own beside them.
const OLD_HOOKS = """{"model":"own","hooks":{
    "Stop":[{"hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-status-idle.sh"},{"type":"command","command":"/usr/local/bin/user-stop.sh"}]}],
    "UserPromptSubmit":[{"hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-status-working.sh"}]}],
    "Notification":[{"hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-status-blocked.sh"}]}]}}"""

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

        unretired = String[]
        @test ShipTools._claude_settings_targets(home, joinpath(home, ".claude"); unretired = unretired) ==
              [realpath(default), realpath(a)]
        # Only the dangling link is a problem; a folder with no settings.json has nothing to retire.
        @test length(unretired) == 1 && occursin(d, only(unretired))
        # An install run from inside an account folder with no settings.json finds the same files.
        @test ShipTools._claude_settings_targets(home, joinpath(auth, "c")) == [realpath(default), realpath(a)]
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

@testset "_retire_claude_hooks: the comm hooks leave every account's real settings.json, links stay links" begin
    # Each real file is rewritten once, links resolved, a person's own hooks and settings kept, a re-run byte-identical.
    # The default here is itself a link to a file outside every claude dir: a copy-then-rename onto the LINK path would
    # replace the link with a plain copy, so this also proves a link stays a link.
    mktempdir() do home
        shared = joinpath(home, "dotfiles", "claude-settings.json")
        mkpath(dirname(shared)); write(shared, OLD_HOOKS)
        default = joinpath(home, ".claude", "settings.json")
        mkpath(dirname(default)); symlink(shared, default)
        auth = joinpath(home, ".claude-auth")
        a = joinpath(auth, "a", "settings.json")
        mkpath(dirname(a)); write(a, OLD_HOOKS)
        b = joinpath(auth, "b", "settings.json")
        mkpath(dirname(b)); symlink(default, b)
        c = joinpath(auth, "c"); mkpath(c)
        in_home(home) do
            ShipTools._retire_claude_hooks(ShipTools.claude_home())
            for f in (shared, a)
                @test every_cmd(f) == ["/usr/local/bin/user-stop.sh"]
                @test jq_lines(`jq -r .model $f`) == ["own"]
                @test jq_lines(`jq -r '.hooks | keys[]' $f`) == ["Stop"]
            end
            @test islink(default) && readlink(default) == shared
            @test islink(b) && readlink(b) == default
            @test isempty(readdir(c))

            # A re-run changes nothing.
            before = Dict(f => read(f) for f in (shared, a))
            ShipTools._retire_claude_hooks(ShipTools.claude_home())
            for f in (shared, a)
                @test read(f) == before[f]
            end
        end
    end
end

@testset "_retire_claude_hooks: a settings.json with no comm hook is not rewritten, whatever its layout" begin
    mktempdir() do home
        f = joinpath(home, ".claude", "settings.json")
        mkpath(dirname(f))
        text = """{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/usr/local/bin/user.sh"}]}]},   "model":"x"}"""
        write(f, text)
        in_home(home) do
            ShipTools._retire_claude_hooks(ShipTools.claude_home())
            @test read(f, String) == text
        end
    end
end

@testset "_retire_claude_hooks: never creates a settings.json" begin
    mktempdir() do home
        mkpath(joinpath(home, ".claude"))
        c = joinpath(home, ".claude-auth", "c"); mkpath(c)
        in_home(home) do
            ShipTools._retire_claude_hooks(c)
            @test isempty(readdir(c))
            @test isempty(readdir(joinpath(home, ".claude")))
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

@testset "_retire_claude_hooks: a rewrite keeps the file's mode and a re-run does not rewrite" begin
    if Sys.iswindows()
        @test_skip false
    else
        mktempdir() do home
            f = joinpath(home, ".claude", "settings.json")
            mkpath(dirname(f)); write(f, OLD_HOOKS)
            chmod(f, 0o604)  # a mode no umask produces
            in_home(home) do
                ShipTools._retire_claude_hooks(joinpath(home, ".claude"))
                @test filemode(f) & 0o777 == 0o604
                @test every_cmd(f) == ["/usr/local/bin/user-stop.sh"]
                ino1 = stat(f).inode
                ShipTools._retire_claude_hooks(joinpath(home, ".claude"))
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

@testset "_retire_claude_hooks: a relative, linked or bare config dir finds the same files and creates nothing" begin
    mktempdir() do home
        default = joinpath(home, ".claude", "settings.json")
        mkpath(dirname(default)); write(default, OLD_HOOKS)
        auth = joinpath(home, ".claude-auth")
        c = joinpath(auth, "c"); mkpath(c)
        symlink(c, joinpath(home, "alias"))
        in_home(home) do
            cd(home) do
                ShipTools._retire_claude_hooks(joinpath(".claude-auth", "c"))
            end
            ShipTools._retire_claude_hooks(joinpath(home, "alias"))
            ShipTools._retire_claude_hooks(auth)
            @test isempty(readdir(c))
            @test !ispath(joinpath(auth, "settings.json"))
            @test every_cmd(default) == ["/usr/local/bin/user-stop.sh"]
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

@testset "_retire_claude_hooks: a file that cannot be rewritten is named, and the other files are done" begin
    mktempdir() do home
        a = joinpath(home, ".claude-auth", "a", "settings.json")
        mkpath(dirname(a)); write(a, OLD_HOOKS)
        ro = joinpath(home, "ro"); mkpath(ro)
        stuck = joinpath(ro, "settings.json"); write(stuck, OLD_HOOKS)
        chmod(stuck, 0o444)   # the staging file takes this mode, so jq cannot write it
        try
            restrained = try open(stuck, "a") do _ end; false catch; true end
            if !restrained
                @test_skip false
            else
                in_home(home) do
                    unretired = String[]
                    ShipTools._retire_claude_hooks(ro; unretired = unretired)
                    @test every_cmd(a) == ["/usr/local/bin/user-stop.sh"]
                    @test length(unretired) == 1 && occursin(realpath(stuck), only(unretired))
                    @test read(stuck, String) == OLD_HOOKS
                    @test sort(readdir(ro)) == ["settings.json"]   # no staging file left
                end
            end
        finally
            chmod(stuck, 0o644)
        end
    end
end

@testset "install: the closing summary names every settings file the old hooks could not be removed from" begin
    mktempdir() do home
        default = joinpath(home, ".claude", "settings.json")
        mkpath(dirname(default)); write(default, "{}")
        lockd = joinpath(home, ".claude-auth", "lockd", "settings.json")
        mkpath(dirname(lockd))
        write(lockd, OLD_HOOKS)
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
                    summaries = filter(r -> occursin("may still be in these Claude settings", string(r.message)), logger.logs)
                    @test length(summaries) == 1
                    msg = string(only(summaries).message)
                    # A file the install could not rewrite is named by the real path it writes (links and short names
                    # resolved); a folder with no settings.json has nothing to retire and is not named.
                    @test occursin(realpath(lockd), msg)
                    @test !occursin(empty, msg)
                    @test !occursin(realpath(default), msg)
                    @test read(lockd, String) == OLD_HOOKS   # left exactly as it was
                end
            end
        finally
            chmod(lockd, 0o644)
        end
    end
end

@testset "install: an account's unreadable settings.json is named in the summary and the install goes on" begin
    mktempdir() do home
        default = joinpath(home, ".claude", "settings.json")
        mkpath(dirname(default)); write(default, "{}")
        acct = joinpath(home, ".claude-auth", "acct", "settings.json")
        mkpath(dirname(acct))
        write(acct, "{}")
        chmod(acct, 0o000)
        try
            restrained = try read(acct); false catch; true end
            if !restrained
                @test_skip false
            else
                in_home(home) do
                    logger = Test.TestLogger(min_level = Base.CoreLogging.Warn)
                    Base.CoreLogging.with_logger(logger) do
                        @test (ShipTools.update_comm(clis = [:claude]); true)   # does not throw
                    end
                    summaries = filter(r -> occursin("may still be in these Claude settings", string(r.message)), logger.logs)
                    @test length(summaries) == 1
                    @test occursin(realpath(acct), string(only(summaries).message))
                end
            end
        finally
            chmod(acct, 0o644)
        end
        @test read(acct, String) == "{}"
    end
end

@testset "_retire_comm_hooks!: an empty or whitespace-only settings.json is left as it is, an unreadable one is skipped" begin
    for (name, text) in (("0-byte", ""), ("whitespace-only", " \n\t\n"))
        mktempdir() do dir
            f = joinpath(dir, "settings.json")
            write(f, text)
            @test ShipTools._retire_comm_hooks!(f)
            @test read(f, String) == text
        end
    end
    mktempdir() do dir
        f = joinpath(dir, "settings.json")
        write(f, "{}")
        chmod(f, 0o000)
        try
            restrained = try read(f); false catch; true end
            if !restrained
                @test_skip false
            else
                @test_logs (:warn, r"could not remove the comm hooks") match_mode = :any begin
                    @test !ShipTools._retire_comm_hooks!(f)
                end
                @test isempty(filter(n -> n != "settings.json", readdir(dir)))   # no staging file left
            end
        finally
            chmod(f, 0o644)
        end
        @test read(f, String) == "{}"
    end
end

@testset "settings.json: a file holding more than one JSON value, or a non-object, is left untouched" begin
    for (name, text) in (("two objects", "{\"a\":1}\n{\"b\":2}\n"), ("false", "false\n"))
        mktempdir() do dir
            f = joinpath(dir, "settings.json")
            write(f, text)
            @test_logs (:warn, r"could not") match_mode = :any begin
                @test !ShipTools._retire_comm_hooks!(f)
            end
            @test read(f, String) == text
            @test isempty(filter(n -> n != "settings.json", readdir(dir)))
        end
    end
end

@testset "_retire_comm_hooks!: every comm hook goes, whatever its event, matcher or quoting" begin
    mktempdir() do dir
        settings = joinpath(dir, "settings.json")
        write(settings, """{"hooks":{
          "PermissionRequest":[{"hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-status-dialog.sh"}]}],
          "PostToolUseFailure":[{"hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-status-heartbeat.sh"}]}],
          "PreToolUse":[
            {"matcher":"Bash","hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-subagent-guard.sh"}]},
            {"matcher":"Edit","hooks":[{"type":"command","command":"'/home/x/.sot-comm/bin/comm-subagent-guard.sh'"}]},
            {"matcher":"Bash","hooks":[{"type":"command","command":"/usr/local/bin/mine.sh"}]}],
          "PostToolUse":[{"hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-status-heartbeat.sh"}]}]}}""")
        @test ShipTools._retire_comm_hooks!(settings)
        @test jq_lines(`jq -r '.hooks | keys[]' $settings`) == ["PreToolUse"]
        @test every_cmd(settings) == ["/usr/local/bin/mine.sh"]
    end
end

@testset "_retire_comm_hooks!: the retired reminder hooks go, a person's hooks stay" begin
    mktempdir() do dir
        settings = joinpath(dir, "settings.json")
        write(settings, """{"hooks":{
          "SessionStart":[
            {"matcher":"compact","hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-postcompact-reminder.sh"}]},
            {"matcher":"clear","hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-postclear-reminder.sh"}]},
            {"matcher":"startup","hooks":[{"type":"command","command":"/usr/local/bin/mine.sh"}]}],
          "Stop":[{"hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-status-idle.sh"}]}],
          "Notification":[{"hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-status-blocked.sh"}]}],
          "PostToolUse":[{"hooks":[{"type":"command","command":"/usr/local/bin/other.sh"}]}]}}""")
        @test ShipTools._retire_comm_hooks!(settings)
        @test sort(every_cmd(settings)) == ["/usr/local/bin/mine.sh", "/usr/local/bin/other.sh"]
        @test jq_lines(`jq -r '.hooks | keys[]' $settings`) == ["PostToolUse", "SessionStart"]
    end
    mktempdir() do dir
        settings = joinpath(dir, "settings.json")
        write(settings, """{"hooks":{"Notification":[{"hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-status-blocked.sh"},{"type":"command","command":"/usr/local/bin/mine.sh"}]}]}}""")
        @test ShipTools._retire_comm_hooks!(settings)
        @test jq_lines(`jq -r '.hooks.Notification[].hooks[].command' $settings`) == ["/usr/local/bin/mine.sh"]
    end
    mktempdir() do dir
        settings = joinpath(dir, "settings.json")
        write(settings, """{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/usr/local/bin/comm-status-report.sh"}]}]}}""")
        before = read(settings, String)
        @test ShipTools._retire_comm_hooks!(settings)
        @test read(settings, String) == before
    end
end
