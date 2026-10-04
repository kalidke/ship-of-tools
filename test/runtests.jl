using ShipTools
using Test

@testset "Ship of Tools" begin
    include("codex_tests.jl")
    include("publish_tests.jl")
    include("skills_tests.jl")
    include("install_tests.jl")
    include("homes_tests.jl")
    include("claude_hooks_tests.jl")
end
