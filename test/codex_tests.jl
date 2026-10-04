# Tests for src/codex.jl: the Codex hooks payload, marketplace registry and home profile export parsing.

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

@testset "codex marketplace payload registry" begin
    # The overwrite guard for ~/.agents/plugins/marketplace.json is a
    # byte-compare against CODEX_MARKETPLACE_PAYLOADS. It used to be
    # `occursin("sot-local", ...)`, which rewrote WHOLESALE any file that
    # merely mentioned the string — including one carrying entries other
    # tools had added. These checks pin the registry the compare relies on.
    payloads = ShipTools.CODEX_MARKETPLACE_PAYLOADS
    @test !isempty(payloads)

    # Every entry is real JSON with the marketplace's top-level shape, and
    # the CURRENT one (what the writer emits) names our marketplace and
    # plugin. jq is the honest parser where available; the scanner check
    # runs everywhere.
    for txt in payloads
        @test sort(ShipTools._json_toplevel_keys(txt)) ==
              ["interface", "name", "plugins"]
    end
    @test occursin("\"sot-local\"", first(payloads))
    @test occursin("\"sot-comm\"", first(payloads))
    if !isnothing(Sys.which("jq"))
        for txt in payloads
            @test success(pipeline(pipeline(IOBuffer(txt), `jq empty`); stderr = devnull))
        end
    end

    # The same-commit convention: a modified file must NOT match the
    # registry, or the guard would overwrite user additions. A plugin
    # appended to our own payload is the exact shape the old guard
    # destroyed.
    modified = replace(
        first(payloads),
        "  ]" => """    ,{ "name": "someone-elses-plugin" }\n  ]""",
    )
    @test modified != first(payloads)   # the replace really landed
    @test !(modified in payloads)
end

@testset "codex home profile export parsing" begin
    # _parse_codex_home_export is pure and file-free by design — never
    # sources or runs a profile — so these exercise it directly on
    # synthetic profile text; no real dotfile is ever touched.
    user, home = "devuser", "/home/devuser"

    # A plain path needs no expansion.
    v, raw = ShipTools._parse_codex_home_export(
        "export CODEX_HOME=/opt/codex-home\n"; user, home)
    @test v == "/opt/codex-home"
    @test raw == "/opt/codex-home"

    # $HOME expands.
    v, raw = ShipTools._parse_codex_home_export(
        "export CODEX_HOME=\$HOME/.codex-work\n"; user, home)
    @test v == "/home/devuser/.codex-work"
    @test raw == "\$HOME/.codex-work"

    # ${USER:-$(id -un)} expands to the current user.
    v, raw = ShipTools._parse_codex_home_export(
        "export CODEX_HOME=/srv/agents/\${USER:-\$(id -un)}/codex\n"; user, home)
    @test v == "/srv/agents/devuser/codex"

    # The `NAME=value; export NAME` two-step form.
    v, raw = ShipTools._parse_codex_home_export(
        "CODEX_HOME=/opt/other; export CODEX_HOME\n"; user, home)
    @test v == "/opt/other"

    # A commented-out line must be ignored entirely.
    v, raw = ShipTools._parse_codex_home_export(
        "# export CODEX_HOME=/should/not/count\n"; user, home)
    @test v === nothing
    @test raw === nothing

    # No assignment anywhere in the file.
    v, raw = ShipTools._parse_codex_home_export(
        "export PATH=\$PATH:/usr/local/bin\nalias ll='ls -la'\n"; user, home)
    @test v === nothing
    @test raw === nothing

    # A value this installer doesn't know how to expand is reported,
    # not guessed at — the raw text survives so a warning can quote it.
    v, raw = ShipTools._parse_codex_home_export(
        "export CODEX_HOME=\$SOME_OTHER_VAR/codex\n"; user, home)
    @test v === nothing
    @test raw == "\$SOME_OTHER_VAR/codex"
end
