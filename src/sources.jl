# Every repo path the installer reads, named once. A move under comm/ edits only this file and comm/bin-folders.txt.

# The repo's comm/ folder, found from this file.
const COMM_SRC = normpath(joinpath(@__DIR__, "..", "comm"))
# The repo root: the folder holding comm/, AGENTS.md and this package.
const REPO_ROOT = dirname(COMM_SRC)
# The list of comm folders whose regular files install flat into `<bin>` (a part inside the file that sources it),
# one repo-relative folder per line.
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
# Every version of a Claude skill the repo ever shipped, one `<name> <hash>` per line (scripts/legacy-skill-hashes.sh).
const LEGACY_SKILL_HASHES = joinpath(COMM_SRC, "legacy-skills.txt")
# The onboarding skills: what a person runs before any row exists, so they install into the global Claude skills folder
# (every account reaches it by the daemon's link) while every other Claude skill is the row layer's alone. The one list.
const CLAUDE_GLOBAL_SKILLS = ["sot-gh-auth", "sot-install", "sot-setup", "sot-statusline-setup"]
# The one file name no install ever copies, from any source folder.
const NEVER_INSTALLED = "CLAUDE.md"
# The conventions file that also installs as $CODEX_HOME/AGENTS.md.
const AGENTS_MD_SRC = joinpath(REPO_ROOT, "AGENTS.md")
