# rust/backend/src/agents: accounts, folder trust and the awareness env (backend agents)

What the daemon sets up around an agent's login before a capsule spawns: which named accounts exist, the folder-trust
record claude would otherwise ask about, and the `SOT_*` awareness env. Part of the backend's agents subsystem; charter:
agents/CLAUDE.md at the repo root (lands with the repo-root agents/ folder).

## Files
- `mod.rs`: declares the three modules and the test support module.
- `accounts.rs`: account discovery, the account name rules, `account_env`, and the shared links a named account gets.
- `folder_trust.rs`: the declared trusted-root prefix and the write of claude's per-folder trust record.
- `awareness.rs`: the `SOT_*` awareness env stamped on every capsule producer, and the daemon's own endpoint path.
- `support_tests.rs`: `touch_dir` and `touch_file`, shared by the accounts and folder-trust tests.

## Start here
`account_env` in `accounts.rs` for what a spawn sets for an account; `ensure_folder_trusted` in `folder_trust.rs` for the
trust record. The old path `crate::accounts::` (rust/backend/src/accounts.rs) re-exports both of those files whole.

## Rules
- A named account never shares the login: only the names in `SHARED_ENTRIES` are linked, by `ensure_account_links`.
- Folder trust is written only for a root under the declared prefix, and an entry already accepted is never rewritten
  (`ensure_folder_trusted`).
- The prefix comes only from the user-level settings file (`trusted_root_prefix`); no environment variable or project
  file declares it.
