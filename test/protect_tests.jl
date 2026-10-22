# Tests for what an install never touches: a person's own skills and hooks, a symlinked skills folder, and the
# messaging protections that stay in every session's settings.

# A skill the repo ships, copied as an earlier install would have placed it.
function place_shipped_copy(skillsroot, name)
    src = first(joinpath(s, name) for s in ShipTools.CLAUDE_SKILL_SRCS if isfile(joinpath(s, name, "SKILL.md")))
    cp(src, joinpath(skillsroot, name))
end
hook_commands(f) = [Sys.iswindows() ? chopsuffix(l, "\r") : l
                    for l in split(read(`jq -r "[(.hooks // {})[] | .[] | (.hooks // [])[] | .command] | .[]" $f`, String), '\n'; keepempty = false)]

@testset "protect: a person's skill under a shipped name survives a first install" begin
    mktempdir() do home
        skills = joinpath(home, ".claude", "skills")
        for n in ("sitrep", "project-log", "worktree")
            mkpath(joinpath(skills, n)); write(joinpath(skills, n, "SKILL.md"), "MY OWN $n")
        end
        place_shipped_copy(skills, "show-result")      # byte for byte a version the repo shipped
        in_home(home) do
            ShipTools.update_comm(clis = [:claude])
            for n in ("sitrep", "project-log", "worktree")
                @test read(joinpath(skills, n, "SKILL.md"), String) == "MY OWN $n"
            end
            @test !ispath(joinpath(skills, "show-result"))
            # A re-run changes nothing more: the rules need no marker.
            ShipTools.update_comm(clis = [:claude])
            @test read(joinpath(skills, "sitrep", "SKILL.md"), String) == "MY OWN sitrep"
            @test !ispath(joinpath(skills, ".sot-skills-moved"))
        end
    end
end

@testset "protect: a symlinked skills folder is never acted through" begin
    mktempdir() do home
        dotfiles = joinpath(home, "dotfiles", "skills")
        mkpath(dotfiles)
        place_shipped_copy(dotfiles, "show-result")
        mkpath(joinpath(home, ".claude"))
        symlink(dotfiles, joinpath(home, ".claude", "skills"))
        in_home(home) do
            logger = Test.TestLogger(min_level = Base.CoreLogging.Warn)
            Base.CoreLogging.with_logger(logger) do
                ShipTools.update_comm(clis = [:claude])
            end
            @test isfile(joinpath(dotfiles, "show-result", "SKILL.md"))
            @test islink(joinpath(home, ".claude", "skills"))
            @test readdir(dotfiles) == ["show-result"]
            @test any(r -> occursin("symbolic link", string(r.message)), logger.logs)
        end
    end
end

@testset "protect: every configuration folder an account uses is migrated, each by the same rule" begin
    mktempdir() do home
        default = joinpath(home, ".claude", "skills"); mkpath(default)
        acct = joinpath(home, ".claude-auth", "a", "skills"); mkpath(acct)
        elsewhere = joinpath(home, "cfg", "skills"); mkpath(elsewhere)
        for d in (default, acct, elsewhere)
            place_shipped_copy(d, "sitrep")
            mkpath(joinpath(d, "mine")); write(joinpath(d, "mine", "SKILL.md"), "MINE")
        end
        in_home(home, "CLAUDE_CONFIG_DIR" => joinpath(home, "cfg")) do
            ShipTools.update_comm(clis = [:claude])
            for d in (default, acct, elsewhere)
                @test !ispath(joinpath(d, "sitrep"))
                @test read(joinpath(d, "mine", "SKILL.md"), String) == "MINE"
            end
        end
    end
end

@testset "protect: a person's hook that mentions an installed comm path survives, an exact old command goes" begin
    mktempdir() do home
        f = joinpath(home, ".claude", "settings.json")
        mkpath(dirname(f))
        write(f, """{"hooks":{
          "UserPromptSubmit":[{"hooks":[
            {"type":"command","command":"\$HOME/.sot-comm/bin/comm-status-working.sh && /usr/local/bin/mine.sh"},
            {"type":"command","command":"\$HOME/.sot-comm/bin/comm-status-working.sh"}]}],
          "Stop":[{"hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-status-report.sh"},
                            {"type":"command","command":"echo \$HOME/.sot-comm/bin/comm-status-idle.sh"}]}]}}""")
        in_home(home) do
            ShipTools.update_comm(clis = [:claude])
            cmds = hook_commands(f)
            @test "\$HOME/.sot-comm/bin/comm-status-working.sh && /usr/local/bin/mine.sh" in cmds
            @test "\$HOME/.sot-comm/bin/comm-status-report.sh" in cmds
            @test "echo \$HOME/.sot-comm/bin/comm-status-idle.sh" in cmds
            @test count(==("\$HOME/.sot-comm/bin/comm-status-working.sh"), cmds) == 0
        end
    end
end

@testset "protect: the mail check and the subagent guard stay in the user's settings, the work-state hooks do not" begin
    mktempdir() do home
        in_home(home) do
            ShipTools.update_comm(clis = [:claude])
            cmds = hook_commands(joinpath(home, ".claude", "settings.json"))
            @test sort(cmds) == sort(["\$HOME/.sot-comm/bin/comm-status-idle.sh", "\$HOME/.sot-comm/bin/comm-subagent-guard.sh"])
        end
    end
end
