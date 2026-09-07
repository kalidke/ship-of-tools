---
description: Pull the ops-sidecar git bus and surface new cross-OS entries since the last sync.
allowed-tools: Bash
---

Pull and show new entries from the other side:

```bash
~/.sot-comm/bin/bus.sh sync
```

Present each new entry to the user (timestamp, host/user, body) without
editorializing. If an entry implies a follow-up action, ask the user what to
do rather than acting on it unilaterally — unless it's an obvious one-liner.
