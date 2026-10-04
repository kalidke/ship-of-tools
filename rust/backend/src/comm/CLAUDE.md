# rust/backend/src/comm: the daemon's half of messaging (backend)

Messaging between sessions is a pair of halves: the comm scripts at the repo's `comm/` and, here, the daemon's part of
delivery and of the address book. Part of the backend; design of record: docs/adr/0049-messaging-on-one-page.md.

## Files
- `mail/`: delivery: the daemon's link to the hub that files relayed messages
- `mod.rs`: declares the two folders
- `registry/`: the address book: which handle names which session

## Start here
`mail/` for delivery, `registry/` for the address book.

## Rules
- A folder here holds the daemon's side only; the scripts' side stays in the repo's `comm/`.
