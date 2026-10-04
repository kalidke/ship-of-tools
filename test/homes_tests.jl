# Tests for src/homes.jl: env-dir resolution.

@testset "env-dir resolution" begin
    # A set-but-empty override must read as unset: taking "" literally
    # yields a relative path that scatters the install into the CWD.
    withenv("CODEX_HOME" => "") do
        @test ShipTools.codex_home() == joinpath(homedir(), ".codex")
    end
    withenv("CODEX_HOME" => "/tmp/sot-test-codex-home") do
        @test ShipTools.codex_home() == "/tmp/sot-test-codex-home"
    end
    withenv("CODEX_HOME" => nothing) do
        @test ShipTools.codex_home() == joinpath(homedir(), ".codex")
    end
    withenv("SOT_COMM_HOME" => "") do
        @test ShipTools.comm_home() == joinpath(homedir(), ".sot-comm")
    end
    withenv("CLAUDE_CONFIG_DIR" => "") do
        @test ShipTools.claude_home() == joinpath(homedir(), ".claude")
    end
    withenv("CLAUDE_CONFIG_DIR" => "/tmp/sot-test-claude-home") do
        @test ShipTools.claude_home() == "/tmp/sot-test-claude-home"
    end
    withenv("CLAUDE_CONFIG_DIR" => nothing) do
        @test ShipTools.claude_home() == joinpath(homedir(), ".claude")
    end
end
