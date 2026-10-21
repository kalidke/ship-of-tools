# rust/log/claude-sdk-helper: Node helper that drives one Claude Agent SDK session (capsule)

A per-capsule Node process that owns one Claude Agent SDK session across many turns and speaks NDJSON (helper-protocol 1)
on stdin and stdout, one JSON object per line in each direction. Part of the capsule; charter: rust/log/CLAUDE.md.
Status: no product path runs it. Only `sot-capsule claude`, rust/log/tests/claude_e2e.rs and claude_rig.rs, and rust.yml's
`p2-e2e` job do; whether to keep or retire it is open.

## Files
- `.gitignore`: keeps `node_modules/` and `dist/` out of the tree.
- `README.md`: the protocol, the turn model and how the capsule invokes the helper.
- `package-lock.json`: pinned dependency tree, installed by `npm ci`.
- `package.json`: scripts (`npm test` builds `dist/` first) and the pinned SDK version.
- `src/`: `main.ts` (stdin ops to SDK `query()` calls, SDK messages to stdout lines), `codec.ts`, `protocol.ts`.
- `test/`: the helper's `*.test.ts` suites.
- `tsconfig.json`: TypeScript build settings; output goes to `dist/`.

## Start here
`src/main.ts` for any change in how ops and SDK messages are translated; README.md first for the wire protocol.

## Rules
- stdout carries protocol lines only; no logs or banners (`src/main.ts`).
- `HELPER_MODEL`, when set, is forwarded as `model` on every `query()`; when unset the CLI's default applies.

## Tests
`npm ci && npm test` with Node 22, then `SOT_HELPER_E2E=1 cargo test -p sot-log --test claude_e2e` from rust/.
