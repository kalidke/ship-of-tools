# ShipTools — install/update sot-comm, the session-to-session messaging system.
#
# Source of truth: comm/ in the repo. install_comm copies the CLI-agnostic core
# scripts to ~/.sot-comm/bin and each per-CLI adapter to that CLI's dir
# (~/.claude/skills, $CODEX_HOME/skills, ~/.local/bin, hooks/plugins).
# Named accounts (owner ruling): a `~/.claude-auth/<name>` account folder
# gets skills and everything else the default `~/.claude` folder carries
# except the login via a SYMLINK the daemon creates at spawn
# (`rust/backend/src/accounts.rs::ensure_account_links`). The one thing the
# installer itself writes under `.claude-auth` is the comm hooks: an account
# holding a REAL settings.json of its own never sees the shared one, so the
# hooks are merged into every account's settings.json that exists, each
# real file once (`_claude_settings_targets`).
# See comm/PROTOCOL.md for the wire contract.
#
# The Claude adapter additionally installs the work-state hooks listed in
# `_COMM_STATE_HOOKS` (each shells out to a comm-status-*.sh script) so an
# agent's state in the state-nav is event-driven — instant and automatic, no
# model cooperation. Wiring them touches every Claude account's
# settings.json, but via NON-clobbering jq merges (_add_comm_hook!) that add
# one entry per event only if absent and preserve every existing hook; if jq
# is missing the exact JSON to add by hand is printed, and if a file won't
# parse it is left alone untouched (a missing file is created fresh in the
# install's own Claude dir only, since there is nothing there to preserve).
#

module ShipTools

include("sources.jl")
include("homes.jl")
include("publish.jl")
include("comm_bin.jl")
include("skills.jl")
include("launchers.jl")
include("claude_hooks.jl")
include("codex.jl")
include("adapters.jl")
include("install.jl")
export install_comm, update_comm

end
