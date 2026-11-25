---
name: sot-chat-pair
description: "Pair a project folder with a Slack channel, or unpair it, with `sot-fe chat pair` and `sot-fe chat unpair`, for this session's own project or another session's project on the same computer. Use when the person asks to set, connect, link or pair a project's Slack channel, or to remove, unpair or move it: \"pair this project with #lab-general\", \"connect project X to the channel #x\", \"set the Slack channels for these projects\", \"unpair project Y from Slack\", \"move project Z to #new\". Not for posting in a channel (sot-chat-post), connecting a Slack workspace, or a direct message."
---

# sot-chat-pair

A project folder has at most one Slack channel, and a channel at most one project. The project's sessions post to that
channel with `sot-fe chat post`, and where the chat delivers mail, new posts in it reach the project's session as mail, which
starts that session when it is idle: a new pairing makes posts in that channel start that project's session.
`sot-fe chat pair` and `sot-fe chat unpair` change that pairing on the computer that runs your sessions, for this
session's project or another session's there, as the Chat drawer's channel list does for a person.

- Pair or unpair only the projects and channels the person named. Do not pair a project because a channel's name looks
  like it.
- A pair never moves a project that already has a channel and never takes a channel another project has: such a pair is
  refused and the pairing is unchanged (a refusal can come after the Slack app joined the channel). Move a project only when the person asked to: unpair it, then pair it.
- Never pair a channel other than the one the person named without the person's yes. When the person names a channel by
  name, or a pairing was refused as `no_such_channel` or `several_channels`, list first and compare (see List below); when
  the name asked for is not in the list, tell the person the nearest names and ask.
- Each change restarts that Slack workspace's chat for a moment. Do not pair and unpair in a loop.
- Tell the person every change the command printed, each pairing you made included: the folder, the channel and whether its
  posts now reach a session as mail, which starts that session when a post comes.

## List

```bash
~/.sot-comm/bin/sot-fe chat channels ['<text>'] [--slack-workspace <id>]
~/.sot-comm/bin/sot-fe chat workspaces
```

A read: it changes nothing. `channels` prints `workspace <id> <name>`, the default workspace (the one `workspaces` marks
`default`, which `pair` takes too) or the one `--slack-workspace <id>` names, then one line for each channel the Slack app can
see: `<id> #<name> public|private app-in|app-out free|taken`. `taken` means a project of this computer holds the channel, the
asking project's own included, so another project's pairing of it is refused. `<text>` keeps the names that contain it. A last
line says when Slack's list was cut: a channel not shown can still be paired by its ID. It lists the private channels the
app is in, so do not repeat them to anyone but the person. `workspaces` prints `<id> default|other running|stopped <name>`.
Pair by the ID the listing printed, and always pass `pair` the `--slack-workspace` of the `workspace <id>` the listing
printed, whether or not the listing named one: a name or an ID is looked up in the workspace `pair` is given, else the default.

## Pair

```bash
~/.sot-comm/bin/sot-fe chat pair <session> '<channel>'
```

- `<session>`: `.` for this session's own project, or another session's: its slug as
  `~/.sot-comm/bin/sot-fe version` lists it (`row <slug>`), or its id.
- `<channel>`: its name (`'#lab-general'` or `lab-general`) or its ID (`C...` or `G...`, the last part of the channel's
  link in Slack). Quote a name that begins with `#`: unquoted, the shell takes the rest of the line for a comment.
- On a computer that connects several Slack workspaces, a channel outside the default workspace is named with
  `--slack-workspace <id>`; a refusal lists the workspaces and their IDs.

Run it with a tool timeout of at least 180 s: it waits up to 150 s. It prints `paired <folder> with #<name> (<id>) in
<workspace>`, with `; new posts there reach that project's session as mail` when they will, or `unchanged: ...` when the
project already had that channel.

## Unpair

```bash
~/.sot-comm/bin/sot-fe chat unpair <session>
```

It prints `unpaired <folder> from #<name> (<id>) in <workspace>`, or `unchanged: <folder> has no channel`. That project's
sessions can no longer post to the channel, and its posts no longer reach them as mail.

## Exit codes

- 0: done, changed or unchanged.
- 1: a usage error; read the message.
- 2: nothing was changed (a listing changes nothing). `refused <code> <words>`: fix what the words name and run it once more, or tell the person
  the words. `already_paired` names the project's channel now: move it only if the person asked to. `channel_taken` and
  `unsupported` are the person's to do in the Chat drawer's channel list (`Ctrl+Shift+k`, `Cmd+Shift+k` on a Mac).
- 3: no answer; the change may have been made. Run the same command again: it is safe and says what is there now.

Test the exit code, not the words.
