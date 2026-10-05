using ShipTools
using Test

# Run `f` with `home` as the home every installer folder, and every child the installer starts, resolves from, on
# every platform: `homedir()` reads HOME on Unix and USERPROFILE on Windows (HOMEDRIVE and HOMEPATH spell it there
# too); the comm home and CODEX_HOME (where the codex CLI the installer may start keeps its state) are under it;
# CLAUDE_CONFIG_DIR is unset; and every other SOT_ variable is unset, so no comm script a test runs reaches the
# session's self file or daemon. `env` pairs apply after that clearing and lose to the home, so a test adds variables
# but never moves its home, and no key is passed twice (withenv restores a repeated key wrongly). Throws before `f`
# if `homedir()` is not `home`, so nothing is installed anywhere else.
function in_home(f, home::AbstractString, env::Pair...)
    drive, path = splitdrive(home)
    vars = Dict{String,Union{Nothing,String}}(k => nothing for k in keys(ENV) if startswith(k, "SOT_"))
    merge!(vars, Dict{String,Union{Nothing,String}}(env...))
    merge!(vars, Dict{String,Union{Nothing,String}}("HOME" => home, "USERPROFILE" => home, "HOMEDRIVE" => drive,
        "HOMEPATH" => path, "SOT_COMM_HOME" => joinpath(home, ".sot-comm"), "CODEX_HOME" => joinpath(home, ".codex"),
        "CLAUDE_CONFIG_DIR" => nothing))
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
                # No subject file names a home variable as a string: a home is set only through in_home.
                for f in filter(f -> endswith(f, ".jl") && f != "runtests.jl", readdir(@__DIR__))
                    @test !occursin(r"\"(HOME|USERPROFILE|HOMEDRIVE|HOMEPATH)\"", read(joinpath(@__DIR__, f), String))
                end
                # in_home's contract, read back on the suite home: the comm home is the one SOT_ variable left, and
                # codex's home is under the home.
                @test filter(startswith("SOT_"), collect(keys(ENV))) == ["SOT_COMM_HOME"]
                @test get(ENV, "CODEX_HOME", "") == joinpath(homedir(), ".codex")
            end
        end
    end
end
