# The resolved runtime homes the installer writes into (sot-comm, codex, claude).

# A set-but-EMPTY override is treated as unset, matching the repo's own shell
# convention (`${SOT_COMM_HOME:-...}`). Taking "" literally would yield a
# relative path that scatters the install into the process CWD, and mkpath("")
# throws partway through — a confusing failure for a trivially recoverable env
# slip.
_env_dir(var, default) = (v = get(ENV, var, ""); isempty(v) ? default : v)

"Resolved runtime home for sot-comm (honors `\$SOT_COMM_HOME`)."
comm_home() = _env_dir("SOT_COMM_HOME", joinpath(homedir(), ".sot-comm"))

# Codex reads its skills, AGENTS.md, config and plugin cache from $CODEX_HOME,
# NOT unconditionally from ~/.codex — and a host that points CODEX_HOME
# elsewhere (e.g. a machine-local dir, to keep per-host state off a shared NFS
# HOME) turned every hardcoded ~/.codex write into a no-op the sessions never
# read: skills and AGENTS.md installed into a directory codex had no interest
# in, while `codex plugin add` — which inherits the env — correctly wrote the
# plugin half to the real CODEX_HOME, leaving the install split across two
# dirs. Honor the env like comm_home() does.
"Resolved runtime home for codex (honors `\$CODEX_HOME`)."
codex_home() = _env_dir("CODEX_HOME", joinpath(homedir(), ".codex"))

"Resolved runtime home for the DEFAULT claude account (honors `\$CLAUDE_CONFIG_DIR`)."
claude_home() = _env_dir("CLAUDE_CONFIG_DIR", joinpath(homedir(), ".claude"))
