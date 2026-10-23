# ShipTools — install/update sot-comm, the session-to-session messaging system.
#
# Source of truth: comm/ in the repo. install_comm copies the CLI-agnostic core
# scripts to ~/.sot-comm/bin, the Claude skills into the Claude layer
# (~/.sot-comm/claude-layer, which only a leg of the launch recipe adds, so a plain
# `claude` loads none of them), and each per-CLI adapter's other parts to that CLI's
# dir ($CODEX_HOME/skills, ~/.local/bin, plugins). The work-state hooks are not
# written into any settings.json: a row's leg carries them in its own `--settings`
# (`rust/backend/src/agents/layer.rs`). Two hooks serve every joined session and stay
# in every Claude account's settings.json (`_COMM_STATE_HOOKS`: the end-of-turn mail
# check and the subagent guard), merged by NON-clobbering jq edits
# (`_add_comm_hook!`) that add one entry per event only if absent and keep every other
# hook (the install stops before them without jq); a file that won't parse is left
# untouched and the exact JSON to add by hand is printed. What an earlier install put
# in the user's own folders is removed only where it is an exact copy: the comm hooks
# it wrote by exact command (`_remove_stale_comm_hooks!`) and the skills it shipped by
# exact content (`_retire_placed_skills!`, against comm/legacy-skills.txt). A named
# account shares the default skills folder by a SYMLINK the daemon creates at spawn
# (`rust/backend/src/agents/accounts.rs::ensure_account_links`); a link is never acted
# through. Settings files are found once each by `_claude_settings_targets`.
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
