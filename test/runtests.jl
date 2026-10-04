using ShipTools
using Test

# Path to the repo's comm/ source (tests run from test/, package root is ..).
const COMM_DIR = normpath(joinpath(@__DIR__, "..", "comm"))

@testset "Ship of Tools" begin
    include("codex_tests.jl")
    include("publish_tests.jl")
    include("skills_tests.jl")
    include("install_tests.jl")
    include("homes_tests.jl")
    include("claude_hooks_tests.jl")
end
