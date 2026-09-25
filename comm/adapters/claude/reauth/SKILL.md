---
name: reauth
description: Move THIS capsule row to another Claude account and resume the same conversation there — the daemon replaces the row's agent with one logged in as the named account and resumes this transcript by id. Use it when the session must keep going on a different subscription/login. The running session ends mid-turn; read the failure modes and the usage cost first.
---

# reauth — same row, same conversation, different login

**This session ends. The row does not.** The daemon that owns the row ends
this leg and starts a new `claude` in it with
`CLAUDE_CONFIG_DIR=~/.claude-auth/<account>`, resuming this exact transcript
by id. The row keeps its workspace id, its slug, its root and its declared
handle, so its comm identity is unchanged — only which login it spends.

One command, after the pre-flight below:

```bash
sot-fe reauth "$SOT_WORKSPACE_ID" <account>
```

It answers `ok reauth_accepted` and *then* the leg is killed — the accept is
written before anything is torn down, so that line is the last thing this
session will ever print. A refusal changes nothing at all: the session that
printed it is still the live one.

## Pre-flight — refuse, do not improvise

Each failure below is a refusal with the reason, not a workaround.

- **No account named.** Print the current one (`$SOT_ACCOUNT`; empty means
  the default login) and stop. Never guess which account was meant.
- **`$SOT_WORKSPACE_ID` unset** — not a capsule row. There is no row for the
  daemon to restart the agent in; killing this session would just end it.
- **`$CLAUDE_CODE_SESSION_ID` unset** — this agent is not claude, so there is
  no transcript id to resume. The id is read from that variable by the verb
  itself; you never pass it.
- **The named account equals `$SOT_ACCOUNT`** — nothing to do. Refuse rather
  than pay a restart for a no-op.

Everything else belongs to the daemon: whether the account is discovered,
whether it is logged in, whether the name is valid, whether this row's agent
is switchable, and **whether the target account can actually open this
transcript** (it reads both folders, so it proves that itself — a miss means
that account folder has its own REAL `projects` instead of the shared
symlink, or that the id is not this conversation's). It refuses with the
exact fix command and lists the accounts it can see. Print that verbatim.
**Never create the account folder to make the refusal go away** — an empty
folder is a valid account with no login, and the switch would strand the
conversation behind a login prompt.

## What the switch costs

- **Everything in flight dies with the leg**: the current turn, every
  subagent, every background job, every armed Monitor. Run this from a quiet
  turn — never while a job you launched is still running.
- **The first turn of the resumed session is `/sot-session-start`** (the
  launch recipe appends it), so comm comes back armed on its own.
- **The prior turns were paid on the old login, and the new account has no
  server-side cache for this conversation** — its first turn re-reads the
  whole transcript as fresh input. A reauth is cheap in wall time and NOT
  cheap in usage: on a long conversation that is a real, one-time bill. Say
  so before switching a large session.
- **The record stays honest**: the transcript is one file both accounts read,
  nothing is copied, and the two logins stay separate.

## If the row does not come back

The new account is recorded on the row before anything is torn down, so every
path that starts a leg for it uses the new login. Check the phase with
`sotd status`; a row resting after an ended leg is revived by opening it in
the frontend, on the new account.
