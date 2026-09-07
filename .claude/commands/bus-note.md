---
description: Append a note to the ops-sidecar git bus (cross-OS Claude-to-Claude durable notes), commit, push.
allowed-tools: Bash
---

Append `$ARGUMENTS` to this side's claude-bus log, commit, and push:

```bash
~/.sot-comm/bin/bus.sh note "$ARGUMENTS"
```

Report the printed commit line to the user. If `$ARGUMENTS` is empty, ask
what to say instead of running this — never push an empty entry.
