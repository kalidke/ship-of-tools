# ShipTools — install/update sot-comm, the session-to-session messaging system.
#
# Source of truth: comm/ in the repo. install_comm copies the CLI-agnostic core
# scripts to ~/.sot-comm/bin, the Claude skills into the Claude layer
# (~/.sot-comm/claude-layer, which only a leg of the launch recipe adds, so a plain
# `claude` loads none of them), and each per-CLI adapter's other parts to that CLI's
# dir ($CODEX_HOME/skills, ~/.local/bin, plugins). The work-state hooks are no
# longer written into any settings.json: a row's leg carries them in its own
# `--settings` (`rust/backend/src/agents/layer.rs`). What an earlier install put in
# the user's own folders is removed: its comm hooks from every Claude account's
# settings.json (`_retire_claude_hooks`, which finds each real file once through
# `_claude_settings_targets`) and, once, the skills it shipped from the Claude skills
# folder (`_move_placed_skills!`). Named accounts share the default skills folder by a
# SYMLINK the daemon creates at spawn
# (`rust/backend/src/agents/accounts.rs::ensure_account_links`).
# See comm/PROTOCOL.md for the wire contract.
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
