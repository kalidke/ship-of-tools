---
description: Append a note to this side's `.claude-bus/from-<os>.md`, commit, push.
allowed-tools: Bash, Edit, Read, Write
---

The user wants to send a note to the *other-OS* Claude session through the repo-mediated bus. The body is in `$ARGUMENTS`.

## Procedure

1. **Detect which side you are on:**
   - If `uname` works and reports `Linux` / `Darwin` → side is `linux` (Mac counts as Linux-flavoured for this purpose).
   - Otherwise (Windows / PowerShell) → side is `windows`.
   - Pick the matching file: `<ops>/claude-bus/from-linux.md` or `<ops>/claude-bus/from-windows.md`.

2. **Find the host name** via `hostname` (or `$env:COMPUTERNAME` on Windows) and the user via `whoami`. Falls back to `unknown` if unavailable.

3. **Compose an entry** with the format from `<ops>/claude-bus/README.md`:
   ```
   ## YYYY-MM-DDTHH:MMZ — <host> · <user>

   <body from $ARGUMENTS>

   ---
   ```
   Use UTC. Append to the bottom of the file. Don't rewrite earlier entries.

4. **Commit and push:**
   - `git add <ops>/claude-bus/from-<side>.md`
   - `git commit -m "bus: note from <side> · <one-line summary of body>"` — use a HEREDOC for the message body if it has newlines. Standard commit footer.
   - `git push` — surface failure (push rejected, network, etc.) to the user verbatim; don't retry blind.

5. **Confirm to the user** with the commit SHA and the body of the note.

## Notes

- Never overwrite or delete previous entries; the log is append-only by convention.
- Don't write to the *other* side's file — that side owns its own log.
- If `$ARGUMENTS` is empty, ask the user what they want to say rather than pushing an empty entry.
- If there are uncommitted changes elsewhere in the worktree, ask the user before bundling them into the bus commit. Better to keep bus commits scoped to the bus.
