# Every repo path the installer reads, named once. A move under comm/ edits only this file and comm/bin-folders.txt.

# The repo's comm/ folder, found from this file.
const COMM_SRC = normpath(joinpath(@__DIR__, "..", "comm"))
# The repo root: the folder holding comm/, AGENTS.md and this package.
const REPO_ROOT = dirname(COMM_SRC)
# The list of comm folders whose regular files install flat into `<bin>`, one repo-relative folder per line.
const COMM_BIN_FOLDERS = joinpath(COMM_SRC, "bin-folders.txt")
# The folders holding the Claude skills (each skill a subfolder with a SKILL.md).
const CLAUDE_SKILL_SRCS = [joinpath(COMM_SRC, "adapters", "claude"), joinpath(REPO_ROOT, "agents", "claude")]
# The Claude launcher scripts, installed into ~/.local/bin.
const CLAUDE_LAUNCHER_SRC = joinpath(REPO_ROOT, "agents", "claude", "bin")
# The Codex adapter folder; the install of the Codex arm is skipped when it is absent.
const CODEX_ADAPTER_SRC = joinpath(COMM_SRC, "adapters", "codex")
# The Codex skills (same shape as the Claude ones).
const CODEX_SKILL_SRC = joinpath(CODEX_ADAPTER_SRC, "skills")
# The Codex launcher scripts, installed into ~/.local/bin.
const CODEX_LAUNCHER_SRC = joinpath(REPO_ROOT, "agents", "codex", "bin")
# The hooks payload that becomes the sot-comm plugin's hooks/hooks.json.
const CODEX_HOOKS_JSON_SRC = joinpath(CODEX_ADAPTER_SRC, "hooks.json")
# The sot-comm plugin folder (its .codex-plugin/plugin.json manifest).
const CODEX_PLUGIN_SRC = joinpath(CODEX_ADAPTER_SRC, "plugin")
# The one file name no install ever copies, from any source folder.
const NEVER_INSTALLED = "CLAUDE.md"
# The conventions file that also installs as $CODEX_HOME/AGENTS.md.
const AGENTS_MD_SRC = joinpath(REPO_ROOT, "AGENTS.md")
