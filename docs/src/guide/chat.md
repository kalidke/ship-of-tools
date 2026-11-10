# The project chat

The Chat drawer (`Ctrl+Shift+K`) shows the messages of one Slack channel: the channel the selected row's folder maps to.
The records the window holds are listed oldest first, and the newest sits at the bottom. The view follows the newest, so
a message that arrives takes the bottom line and lifts the rest. An empty channel shows `no messages`.

Each message starts with its own header line: the author's name, a tag for a post that came from the drawer, a row or a
bot, and the time (`HH:MM` for today, with the date for an earlier day). The text follows as typed.

## Which channel

The hub's `chat.toml` maps a folder to a channel; a row whose folder is mapped shows that channel. How the mapping is
written is in [the chat configuration](../ref/config.md#chat.toml-and-chat-secrets.toml). The drawer follows the selected
row; for a row whose folder is unmapped, or while the hub has no chat, it shows `no messages`.

## Markup shows as markup

A message is shown as its poster typed it. Slack's markup, such as a mention or a link, is shown as the markup text in
its own color and is not rendered: a link is not clickable and a mention is not replaced by a name. Text that looks like
a header inside a message stays body text.

## When Slack is unreachable

A break in the daemon's Slack connection shows only when it costs the window a snapshot of the channel; the drawer keeps
the records it holds until a new snapshot replaces them.

## Not yet

Posting from the drawer, older pages of history, threads and sharing a selection with the agent are not built yet. Each
arrives with its own change, and this page says so when it does.
