---
description: Pull, then surface new entries from the other side's `.claude-bus/from-<os>.md`.
allowed-tools: Bash, Read, Write
---

The user wants to see what the other-OS Claude session has said since the last sync. The bus lives in the PRIVATE ops sidecar repo — resolve its checkout as `$SOT_OPS_DIR` if set, else the sibling `../ship-of-tools-ops` (relative to this repo's root; shared-`$HOME` Linux boxes and Windows both keep it as a sibling clone). The bus dir is `<ops>/claude-bus/`; conventions are in its README.

## Procedure

1. **Pull the OPS repo** (`$SOT_OPS_DIR` or `../ship-of-tools-ops`): `git pull --rebase` there — NOT the product repo. If there are uncommitted changes that would conflict, stop and tell the user — don't auto-stash. If the pull fails (no upstream, network error), surface the error verbatim.

2. **Detect which side you are on** the same way `/bus-note` does (uname → linux/darwin; otherwise windows). The *other* side is the one to read.

3. **Read the cursor:** `<ops>/claude-bus/.cursor-<this-side>` (gitignored, per-machine). It holds the timestamp of the most recent entry from the other side that you've already shown the user. If absent, treat as "epoch" — everything is new.

4. **Parse the other side's file**:
   - Open `<ops>/claude-bus/from-<other-side>.md`.
   - Each entry starts with `## YYYY-MM-DDTHH:MMZ — …` and ends at the next `---` separator (or EOF).
   - Collect entries whose timestamp is *strictly newer* than the cursor.

5. **Present the new entries to the user**:
   - If none, say so concisely ("bus is quiet — no new entries from <other-side>").
   - If some, render each in a compact form: timestamp + host/user + body. Don't editorialise; let the user see the actual content.
   - If an entry implies a follow-up action (a question, a build report, a request), prompt the user on what to do — don't unilaterally act on bus contents unless they're obvious one-liners (e.g. "pull and rebuild").

6. **Advance the cursor:** write the timestamp of the newest entry you showed into `<ops>/claude-bus/.cursor-<this-side>`. Always one entry, no history.

## Notes

- `/bus-sync` is the building block for `/loop /bus-sync`. The loop skill will call this command on a schedule; the cursor file is what prevents re-surfacing the same entry every interval.
- Never *modify* `<ops>/claude-bus/from-<other-side>.md` from this side; reads only.
- Cursor format: a single ISO-8601 timestamp on one line, no JSON, no frills.
- If the cursor file's content is malformed, treat it as "epoch" and overwrite when done; warn the user but keep going.
