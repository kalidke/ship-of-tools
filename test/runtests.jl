using ShipTools
using Test

# Path to the repo's comm/ source (tests run from test/, package root is ..).
const COMM_DIR = normpath(joinpath(@__DIR__, "..", "comm"))

@testset "Ship of Tools" begin
    @testset "codex hooks payload" begin
        # This file has died silently twice — an unrecognized top-level key
        # (codex rejects the WHOLE file) and a wrong event-key case (the key is
        # dropped). Neither reports an error anywhere: the only symptom is that
        # every codex session stops reporting its work-state, which reads as a
        # Ship of Tools bug rather than a parse failure. These checks are the
        # cheap standing guard; see comm/adapters/codex/hooks.README.md.
        hooks_path = joinpath(COMM_DIR, "adapters", "codex", "hooks.json")
        @test isfile(hooks_path)
        txt = read(hooks_path, String)

        # (1) Real JSON. Also covers the scanner's one blind spot: on malformed
        # input a stray key can sit at depth <= 0 and escape detection.
        @test_nowarn ShipTools._json_toplevel_keys(txt)
        if !isnothing(Sys.which("jq"))
            @test success(pipeline(`jq empty $hooks_path`; stderr = devnull))
        end

        # (2) EXACTLY one top-level key. The struct is deny_unknown_fields, so
        # anything else here silently disables every hook.
        @test ShipTools._json_toplevel_keys(txt) == ["hooks"]

        # (3) Event keys are PascalCase. serde(rename = "...") with no alias, in
        # 0.142.5 and 0.146 alike, so a snake_case key is an unrecognized inner
        # key and is dropped in silence.
        for ev in ("UserPromptSubmit", "PostToolUse", "Stop", "PermissionRequest")
            @test occursin("\"$ev\"", txt)
        end
        # A snake_case twin is not a harmless fallback: if it ever became a
        # serde alias, both keys present would be a fatal duplicate-field parse
        # error — the same total silent failure, reintroduced.
        for ev in ("user_prompt_submit", "post_tool_use", "permission_request")
            @test !occursin("\"$ev\"", txt)
        end

        # (4) Every hook command must be a script install_comm actually deploys
        # into $SOT_COMM_HOME/bin, so a rename can't leave the payload pointing
        # at a file that never arrives. Sources are searched across adapters on
        # purpose: three of the four scripts the CODEX payload references
        # (comm-status-{working,heartbeat,idle}.sh) ship with the CLAUDE
        # adapter, so `install_comm(clis = [:codex])` on its own would deploy
        # hooks pointing at scripts it never installed. Harmless under the
        # default clis = [:claude, :codex]; worth knowing before anyone splits
        # them.
        sources = String[]
        for (root, _, files) in walkdir(COMM_DIR), f in files
            endswith(f, ".sh") && push!(sources, f)
        end
        for m in eachmatch(r"\$HOME/\.sot-comm/bin/([A-Za-z0-9._-]+)", txt)
            @test m.captures[1] in sources
        end
    end

    @testset "_json_toplevel_keys" begin
        f = ShipTools._json_toplevel_keys
        @test f("{\"hooks\": {\"Stop\": []}}") == ["hooks"]
        # Format-independent: a regex over indented key lines silently stopped
        # matching when the file was reformatted, which is why this walks text.
        @test f("{\"hooks\":{\"Stop\":[]}}") == ["hooks"]
        @test f("{\n\t\"hooks\": {}\n}") == ["hooks"]
        # Stray keys are caught wherever they sit.
        @test f("{\"_comment\": \"x\", \"hooks\": {}}") == ["_comment", "hooks"]
        # Not fooled by structure inside string values.
        @test f("{\"hooks\": {\"S\": \"{a:b}\"}}") == ["hooks"]
        @test f("{\"hooks\": {\"S\": \"he said \\\"hi\\\": ok\"}}") == ["hooks"]
        # A depth-1 string VALUE must not be mistaken for a key.
        @test f("{\"hooks\": \"x\", \"y\": 1}") == ["hooks", "y"]
    end

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
    end
end
