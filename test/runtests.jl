using ShipTools
using Test

# Run `f` with `home` as the home every installer folder resolves from, on every platform: `homedir()` reads HOME on
# Unix and USERPROFILE on Windows (HOMEDRIVE and HOMEPATH spell it there too), the comm home is under it, and
# CLAUDE_CONFIG_DIR and CODEX_HOME are unset. `env` pairs lose to these, so a test adds variables but never moves its
# home, and no key is passed twice (withenv restores a repeated key wrongly). Throws before `f` if `homedir()` is not
# `home`, so nothing is installed anywhere else.
function in_home(f, home::AbstractString, env::Pair...)
    drive, path = splitdrive(home)
    vars = Dict{String,Union{Nothing,String}}(env...)
    merge!(vars, Dict{String,Union{Nothing,String}}("HOME" => home, "USERPROFILE" => home, "HOMEDRIVE" => drive,
        "HOMEPATH" => path, "SOT_COMM_HOME" => joinpath(home, ".sot-comm"), "CLAUDE_CONFIG_DIR" => nothing,
        "CODEX_HOME" => nothing))
    withenv(vars...) do
        homedir() == home || error("in_home: homedir() is $(homedir()), not $home")
        f()
    end
end

# The whole suite runs in a home of its own, so a test that sets no home still never writes in the real one.
mktempdir() do suite_home
    in_home(suite_home) do
        @testset "Ship of Tools" begin
            include("codex_tests.jl")
            include("publish_tests.jl")
            include("skills_tests.jl")
            include("install_tests.jl")
            include("homes_tests.jl")
            include("claude_hooks_tests.jl")

            @testset "a test sets its home only through in_home" begin
                for f in filter(f -> endswith(f, ".jl") && f != "runtests.jl", readdir(@__DIR__))
                    @test !occursin(r"\"(HOME|USERPROFILE|HOMEDRIVE|HOMEPATH)\"\s*=>", read(joinpath(@__DIR__, f), String))
                end
            end
        end
    end
end
