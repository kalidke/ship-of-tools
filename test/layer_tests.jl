# Tests for the Claude layer (src/skills.jl, src/adapters.jl): the skills exist only where a leg of the launch recipe runs.
# A skill reaches the comm home's claude-layer and no skills folder of the user's; test/protect_tests.jl holds what an
# install removes and what it leaves.

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
            # Not in the Claude skills folder.
            @test !ispath(joinpath(home, ".claude", "skills"))
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
