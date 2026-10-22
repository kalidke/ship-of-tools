# Tests for the Claude layer (src/skills.jl, src/claude_hooks.jl, src/adapters.jl): the skills and hooks exist only
# where a leg of the launch recipe runs. A skill reaches the comm home's claude-layer and no settings file or skills
# folder of the user's; an install removes what an earlier install placed there and nothing else.

jq_values(cmd::Cmd) = [Sys.iswindows() ? chopsuffix(l, "\r") : l for l in split(read(cmd, String), '\n'; keepempty = false)]
# The hook commands settings file `f` runs, over every event.
all_hook_commands(f) = jq_values(`jq -r "[(.hooks // {})[] | .[] | (.hooks // [])[] | .command] | .[]" $f`)

# The names of the skills the repo ships for Claude.
shipped_skills() = sort(unique([n for src in ShipTools.CLAUDE_SKILL_SRCS for n in readdir(src)
                                if isfile(joinpath(src, n, "SKILL.md"))]))

layer_skills(home) = joinpath(home, ".sot-comm", "claude-layer", ".claude", "skills")

@testset "layer: every Claude skill lands in the comm home's layer, and the user's own folders get none" begin
    mktempdir() do home
        in_home(home) do
            ShipTools.update_comm(clis = [:claude])
            names = shipped_skills()
            @test !isempty(names)
            for n in names
                @test isfile(joinpath(layer_skills(home), n, "SKILL.md"))
            end
            # Not in the Claude skills folder, and no settings file made or written for hooks.
            @test !ispath(joinpath(home, ".claude", "skills"))
            @test !ispath(joinpath(home, ".claude", "settings.json"))
        end
    end
end

@testset "layer: a skill the release stopped shipping leaves the layer, and a re-run changes nothing" begin
    mktempdir() do home
        in_home(home) do
            stale = joinpath(layer_skills(home), "retired-skill")
            mkpath(stale); write(joinpath(stale, "SKILL.md"), "OLD")
            ShipTools.update_comm(clis = [:claude])
            @test !ispath(stale)
            before = Dict(n => read(joinpath(layer_skills(home), n, "SKILL.md")) for n in shipped_skills())
            ShipTools.update_comm(clis = [:claude])
            for n in shipped_skills()
                @test read(joinpath(layer_skills(home), n, "SKILL.md")) == before[n]
            end
        end
    end
end

@testset "retire: the comm hooks an earlier install wrote leave every settings file, a person's hooks stay" begin
    mktempdir() do home
        old = """{"model":"own","hooks":{
            "Stop":[{"hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-status-idle.sh"},{"type":"command","command":"/usr/local/bin/user-stop.sh"}]}],
            "UserPromptSubmit":[{"hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-status-working.sh"}]}],
            "PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-subagent-guard.sh"}]},
                          {"matcher":"Bash","hooks":[{"type":"command","command":"/usr/local/bin/user-guard.sh"}]}],
            "PostToolUse":[{"hooks":[{"type":"command","command":"\$HOME/.sot-comm/bin/comm-status-heartbeat.sh"}]}]}}"""
        shared = joinpath(home, "dotfiles", "settings.json")
        mkpath(dirname(shared)); write(shared, old)
        default = joinpath(home, ".claude", "settings.json")
        mkpath(dirname(default)); symlink(shared, default)
        a = joinpath(home, ".claude-auth", "a", "settings.json")
        mkpath(dirname(a)); write(a, old)
        untouched = joinpath(home, ".claude-auth", "b", "settings.json")
        mkpath(dirname(untouched)); write(untouched, """{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/usr/local/bin/mine.sh"}]}]}}""")
        before = read(untouched)
        in_home(home) do
            ShipTools.update_comm(clis = [:claude])
            for f in (shared, a)
                @test sort(all_hook_commands(f)) == ["/usr/local/bin/user-guard.sh", "/usr/local/bin/user-stop.sh"]
                @test jq_values(`jq -r .model $f`) == ["own"]
            end
            @test islink(default) && readlink(default) == shared
            @test read(untouched) == before
            @test !ispath(joinpath(home, ".claude-auth", "c"))
        end
    end
end

@testset "move: the skills an earlier install shipped leave the Claude skills folder once, a person's own stay" begin
    mktempdir() do home
        skills = joinpath(home, ".claude", "skills")
        for n in ("sitrep", "show-result", "sot-comm", "sot-be-session-start")   # shipped, or retired by name
            mkpath(joinpath(skills, n)); write(joinpath(skills, n, "SKILL.md"), "EARLIER")
        end
        mkpath(joinpath(skills, "my-own")); write(joinpath(skills, "my-own", "SKILL.md"), "MINE")
        in_home(home) do
            ShipTools.update_comm(clis = [:claude])
            for n in ("sitrep", "show-result", "sot-comm", "sot-be-session-start")
                @test !ispath(joinpath(skills, n))
            end
            @test read(joinpath(skills, "my-own", "SKILL.md"), String) == "MINE"
            marker = joinpath(skills, ".sot-skills-moved")
            @test isfile(marker)
            @test Set(split(read(marker, String))) == Set(["sitrep", "show-result", "sot-comm", "sot-be-session-start"])

            # A skill a person makes later under a shipped name is theirs: the marker says the move is done.
            mkpath(joinpath(skills, "sitrep")); write(joinpath(skills, "sitrep", "SKILL.md"), "LATER-MINE")
            ShipTools.update_comm(clis = [:claude])
            @test read(joinpath(skills, "sitrep", "SKILL.md"), String) == "LATER-MINE"
        end
    end
end

@testset "move: a Claude skills folder with nothing of ours in it gets no marker and no change" begin
    mktempdir() do home
        skills = joinpath(home, ".claude", "skills")
        mkpath(joinpath(skills, "my-own")); write(joinpath(skills, "my-own", "SKILL.md"), "MINE")
        in_home(home) do
            ShipTools.update_comm(clis = [:claude])
            @test readdir(skills) == ["my-own"]
        end
    end
end
