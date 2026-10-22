# Tests for the onboarding skills: what a person runs before any row exists, so they stay in the global skills folder
# (and reach every account through the daemon's link) while every other skill is the row layer's alone.

# The names of every skill the repo ships for Claude.
all_shipped_skills() = sort(unique([n for src in ShipTools.CLAUDE_SKILL_SRCS for n in readdir(src)
                                    if isfile(joinpath(src, n, "SKILL.md"))]))

@testset "global skills: one list names the onboarding skills, and each is a shipped skill" begin
    @test sort(ShipTools.CLAUDE_GLOBAL_SKILLS) == ["sot-gh-auth", "sot-install", "sot-setup", "sot-statusline-setup"]
    @test issubset(ShipTools.CLAUDE_GLOBAL_SKILLS, all_shipped_skills())
end

@testset "global skills: an install puts them in the global skills folder, and the layer holds only the others" begin
    mktempdir() do home
        # A named account shares the default skills folder by the link the daemon makes at its first spawn.
        mkpath(joinpath(home, ".claude-auth", "a"))
        mkpath(joinpath(home, ".claude"))
        symlink(joinpath(home, ".claude", "skills"), joinpath(home, ".claude-auth", "a", "skills"))
        # An earlier build of this branch put one of them in the layer: it goes.
        stale = joinpath(home, ".sot-comm", "claude-layer", ".claude", "skills", "sot-install")
        mkpath(stale); write(joinpath(stale, "SKILL.md"), "OLD")
        in_home(home) do
            ShipTools.update_comm(clis = [:claude])
            layer = joinpath(home, ".sot-comm", "claude-layer", ".claude", "skills")
            for n in ShipTools.CLAUDE_GLOBAL_SKILLS
                @test isfile(joinpath(home, ".claude", "skills", n, "SKILL.md"))
                @test isfile(joinpath(home, ".claude-auth", "a", "skills", n, "SKILL.md"))
                @test !ispath(joinpath(layer, n))
            end
            others = setdiff(all_shipped_skills(), ShipTools.CLAUDE_GLOBAL_SKILLS)
            @test sort(readdir(layer)) == others
            for n in others
                @test !ispath(joinpath(home, ".claude", "skills", n))
            end
            # A re-run changes nothing.
            before = read(joinpath(home, ".claude", "skills", "sot-install", "SKILL.md"))
            ShipTools.update_comm(clis = [:claude])
            @test read(joinpath(home, ".claude", "skills", "sot-install", "SKILL.md")) == before
        end
    end
end

@testset "global skills: the removal of shipped skills never takes an onboarding skill" begin
    mktempdir() do home
        skills = joinpath(home, "skills"); mkpath(skills)
        for n in (ShipTools.CLAUDE_GLOBAL_SKILLS..., "sitrep")
            src = first(joinpath(s, n) for s in ShipTools.CLAUDE_SKILL_SRCS if isfile(joinpath(s, n, "SKILL.md")))
            cp(src, joinpath(skills, n))
        end
        ShipTools._retire_placed_skills!(skills)
        for n in ShipTools.CLAUDE_GLOBAL_SKILLS
            @test isfile(joinpath(skills, n, "SKILL.md"))
        end
        @test !ispath(joinpath(skills, "sitrep"))
    end
end
