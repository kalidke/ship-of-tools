---
name: sot-chat-pair
description: "Unpair a project folder from its Slack channel with `sot-fe chat unpair`, for this session's own project or another session's project on the same computer. Use when the person asks to remove, unpair, disconnect or unlink a project's Slack channel: \"unpair project Y from Slack\", \"remove this project's Slack channel\", \"disconnect the channel from project Z\". Not for pairing a project with a channel (the person does that in the Chat drawer's channel list), posting in a channel (sot-chat-post), connecting a Slack workspace, or a direct message."
---

# sot-chat-pair

A project folder has at most one Slack channel, and a channel at most one project. The project's sessions post to that
channel with `sot-fe chat post`, and where the chat delivers mail, new posts in it reach the project's session as mail.
`sot-fe chat unpair` removes that pairing on the computer that runs your sessions, for this session's project or another
session's there, as the Chat drawer's channel list does for a person. Pairing a project with a channel is not a session's
to do yet: the person picks the channel in the Chat drawer's channel list (`Ctrl+Shift+k`, `Cmd+Shift+k` on a Mac).

- Unpair only the projects the person named. Do not unpair a project because its channel looks unused.
- After an unpair that project's sessions can no longer post to the channel, and its posts no longer reach them as mail.
- Each change restarts that Slack workspace's chat for a moment. Do not unpair in a loop.
- Tell the person every change the command printed: the folder and the channel it lost.

## Unpair

```bash
~/.sot-comm/bin/sot-fe chat unpair <session>
```

- `<session>`: `.` for this session's own project, or another session's: its slug as
  `~/.sot-comm/bin/sot-fe version` lists it (`row <slug>`), or its id.

Run it with a tool timeout of at least 180 s: it waits up to 150 s. It prints `unpaired <folder> from #<name> (<id>) in
<workspace>`, or `unchanged: <folder> has no channel` when the project had none.

## Exit codes

- 0: done, changed or unchanged.
- 1: a usage error; read the message.
- 2: nothing was changed. `refused <code> <words>`: fix what the words name (a session that is not listed, the default
  session) and run it once more, or tell the person the words. `unsupported` means the computer that runs your sessions
  is older: the person unpairs in the Chat drawer's channel list.
- 3: no answer; the change may have been made. Run the same command again: it is safe and says what is there now.

Test the exit code, not the words.
