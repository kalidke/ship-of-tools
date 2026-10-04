# The per-CLI adapter install: what the Claude and Codex arms copy and wire.

function _install_adapter(cli::Symbol; unhooked::Vector{String} = String[])
    problems = String[]
    if cli === :claude
        for srcdir in CLAUDE_SKILL_SRCS
            _stage!(problems, "claude skills") do
                _install_skills(srcdir, joinpath(claude_home(), "skills"))
            end
        end
        _stage!(problems, "claude launchers") do
            _install_launchers(CLAUDE_LAUNCHER_SRC)
        end
        _stage!(problems, "claude hooks") do
            _install_claude_hooks(claude_home(); unhooked = unhooked)
        end
        # Named accounts get skills via the shared-folder symlink the
        # daemon creates at spawn (`rust/backend/src/accounts.rs::
        # ensure_account_links`); the hooks are merged into every
        # account's own settings.json by `_install_claude_hooks` itself,
        # since an account with a real settings.json never sees the
        # shared one.
    elseif cli === :codex
        # ADR 0031 — codex adapter: ccx launcher, and the hooks.json plugin
        # payload (state-nav wiring).
        home = codex_home()
        @info "Installed into codex home" dir = home
        mismatch = _codex_home_profile_mismatch(home)
        if mismatch !== nothing
            value, raw, path = mismatch
            if value === nothing
                @warn """
                    a login profile ($path) exports CODEX_HOME=$raw, which this installer \
                    could not resolve with confidence — it may name a different codex home \
                    than the one just installed into ($home). Resolve the value and re-run \
                    under it: CODEX_HOME=<resolved value> julia --project=. -e 'using ShipTools; ShipTools.update_comm()'""" profile = path raw = raw installed = home
            else
                @warn """
                    a login profile exports a different CODEX_HOME than this install used. \
                    Login profile ($path) resolves to $value; installed into $home instead — \
                    a daemon-spawned or non-interactive shell doesn't source that profile, so \
                    it sees the unset-environment default while an interactive login shell \
                    sees the profile's value. Re-run under that environment: \
                    CODEX_HOME=$value julia --project=. -e 'using ShipTools; ShipTools.update_comm()'""" profile = path resolved = value installed = home
            end
        end
        isdir(CODEX_ADAPTER_SRC) || return nothing
        # Same function as the Claude adapter: a Codex skill's resource files
        # travel too, which the old one-entry-per-skill list never carried.
        isdir(CODEX_SKILL_SRC) && _stage!(problems, "codex skills") do
            _install_skills(CODEX_SKILL_SRC, joinpath(codex_home(), "skills"))
        end
        _stage!(problems, "codex launchers") do
            _install_launchers(CODEX_LAUNCHER_SRC)
        end
        # Global codex memory: our AGENTS.md also installs as
        # $CODEX_HOME/AGENTS.md so conventions reach codex sessions in ANY
        # project — workspaces are arbitrary repos that won't carry our file
        # (found live: the first daemon-booted codex reported "AGENTS.md
        # conventions not found" from a scratch workspace). Marker-guarded like
        # hooks.json.
        isfile(AGENTS_MD_SRC) && _stage!(problems, "codex AGENTS.md") do
            dstdir = codex_home()
            mkpath(dstdir)
            dst = joinpath(dstdir, "AGENTS.md")
            marker = "Codex sessions in Ship of Tools"
            if !isfile(dst) || occursin(marker, read(dst, String))
                install_file(AGENTS_MD_SRC, dst)
                @info "Installed global codex AGENTS.md" file = dst
            else
                @warn "codex AGENTS.md exists and is not ours — merge manually" file = dst
            end
        end
        # Hooks deploy as a LOCAL PLUGIN (found the hard way, 2026-07-06:
        # codex 0.142 loads lifecycle hooks from config and ENABLED PLUGINS —
        # a standalone hooks.json in the codex home is silently ignored, which
        # was the "session coloring isn't working" bug). Layout: the implicit
        # local marketplace ~/.agents/plugins/marketplace.json + a sot-comm
        # plugin dir (manifest + hooks/hooks.json), then `codex plugin add`
        # (which re-copies into $CODEX_HOME/plugins/cache on every run, so a
        # repo edit does propagate). Paths in marketplace.json resolve from TWO
        # levels above the file ($HOME) — the marketplace stays HOME-relative
        # even when CODEX_HOME points elsewhere, which is why only skills and
        # AGENTS.md above needed codex_home().
        #
        # Hook TRUST: persisted per hook HASH in $CODEX_HOME/config.toml under
        # [hooks.state], granted once via the /hooks TUI. NOT covered by a
        # shared $HOME when a host points CODEX_HOME at a machine-local path —
        # then trust is per-machine. Unset (the default, ~/.codex) it follows
        # the shared $HOME like everything else. ccx additionally passes
        # --dangerously-bypass-hook-trust so a changed hash can't silently
        # disable state reporting on daemon-spawned sessions.
        #
        # The two silent-death traps this file has already fallen into (both
        # cost weeks of colourless sessions, both report Installed=0 in /hooks
        # with no error anywhere) are written up in
        # comm/adapters/codex/hooks.README.md — read it before editing
        # hooks.json. The guard below enforces the first one.
        isfile(CODEX_HOOKS_JSON_SRC) && _stage!(problems, "codex plugin") do
            pdir = joinpath(homedir(), ".agents", "plugins", "sot-comm")
            mkpath(joinpath(pdir, ".codex-plugin"))
            mkpath(joinpath(pdir, "hooks"))
            install_file(joinpath(CODEX_PLUGIN_SRC, ".codex-plugin", "plugin.json"),
                         joinpath(pdir, ".codex-plugin", "plugin.json"))
            txt = replace(read(CODEX_HOOKS_JSON_SRC, String), "\$HOME" => homedir())
            # codex REJECTS the whole hooks file — every event, silently — if it
            # carries ANY unrecognized top-level key (the config struct is
            # deny_unknown_fields). A "_comment" key documenting the file did
            # exactly that, for weeks. Fail loudly here instead of shipping a
            # file that installs nothing.
            #
            # The scanner does NOT validate JSON syntax, and on malformed input
            # (unbalanced braces) a stray key can sit at depth <= 0 and be
            # missed — measured, not hypothetical. So require the "hooks" key to
            # be PRESENT as well: that fails closed on truncated, array-rooted
            # or empty files, and on any future scanner bug. test/runtests.jl
            # additionally parses the file for real.
            ks = _json_toplevel_keys(txt)
            "hooks" in ks || error("""
                codex hooks.json has no top-level "hooks" key — the file is truncated,
                mangled, or not the shape codex expects. Nothing would install and no
                session would ever show its work-state.""")
            stray = filter(!=("hooks"), ks)
            isempty(stray) || error("""
                codex hooks.json has unrecognized top-level key(s): $(join(stray, ", ")).
                codex silently rejects the ENTIRE file when one is present — every hook
                would report Installed=0 and no session would ever show its work-state.
                Only "hooks" may appear at the top level; put prose in
                comm/adapters/codex/hooks.README.md instead.""")
            write(joinpath(pdir, "hooks", "hooks.json"), txt)
            mp = joinpath(homedir(), ".agents", "plugins", "marketplace.json")
            # Replace the file only when it is absent or byte-identical to a
            # version we wrote (see CODEX_MARKETPLACE_PAYLOADS). Fail closed on
            # anything else — a modified file may hold entries other tools or
            # the user added, and uncertainty is not permission to delete them.
            if !isfile(mp) || read(mp, String) in CODEX_MARKETPLACE_PAYLOADS
                write(mp, first(CODEX_MARKETPLACE_PAYLOADS))
            else
                @warn """
                    ~/.agents/plugins/marketplace.json exists with content this installer
                    did not write, so it was left alone. Ensure it carries the sot-comm
                    plugin entry (source path ./.agents/plugins/sot-comm) — or delete the
                    file and re-run update_comm() to regenerate it.""" file = mp
            end
            if isnothing(Sys.which("codex"))
                # Codex is OPTIONAL (maintainer, 2026-07-11): a machine
                # without the codex CLI is a normal configuration, not a
                # problem — @info, never @warn, so launchers/installers that
                # surface warnings don't read as complaining on every sync.
                @info "codex CLI not installed (optional) — plugin files staged; if you later install codex, run `codex plugin add sot-comm@sot-local`" plugin = pdir
            else
                ok = success(pipeline(`codex plugin add sot-comm@sot-local`; stdout = devnull, stderr = devnull))
                # Name the trust step on the SUCCESS path too: a hook whose
                # definition changed is untrusted again, and an untrusted hook
                # is silently inert in any session not launched by ccx (which
                # passes --dangerously-bypass-hook-trust). Trust is per
                # $CODEX_HOME, so a machine-local CODEX_HOME means per-machine.
                @info """Installed codex hooks plugin — verify in codex with /hooks: \
                    the four events should read Installed=1, and Active=1 once trusted \
                    (press `t` there; needed once per machine)""" plugin = pdir added = ok codex_home = codex_home()
                ok || @warn "codex plugin add failed or already installed — check `codex plugin list` / trust via /hooks"
            end
        end
    else
        @warn "No adapter for this CLI yet — add comm/adapters/$(cli)/ and a case here" cli
    end
    isempty(problems) || error(join(problems, "; "))
    return nothing
end
