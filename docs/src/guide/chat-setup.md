# Setting up Slack for Ship of Tools

Ship of Tools can show a Slack channel next to each project, in the Chat drawer (`Ctrl+K`, `Cmd+K` on a Mac), and post to it. This page
sets that up from nothing. You need to be allowed to add apps to your Slack workspace.

The chat runs on one computer: the one that runs your project sessions. If you use Ship of Tools on one computer, that
is the one, with no `hosts.toml` needed. If you have several, it is the one your `hosts.toml` names as `hub`. Every
window shows the chat of that computer. The files below go on it.

## 1. Make a Slack app

Ship of Tools talks to Slack as an app of your own.

1. Go to [api.slack.com/apps](https://api.slack.com/apps), choose **Create New App**, then **From a manifest**, and pick your workspace.
2. Paste this, then **Create**:

   ```yaml
   display_information:
     name: Ship of Tools
   features:
     bot_user:
       display_name: Ship of Tools
       always_online: true
   oauth_config:
     scopes:
       bot:
         - channels:history
         - channels:read
         - groups:history
         - groups:read
         - users:read
         - chat:write
         - chat:write.customize
   settings:
     event_subscriptions:
       bot_events:
         - message.channels
         - message.groups
     socket_mode_enabled: true
   ```

   What these allow: read public and private channels the app is in and their names, look up people's names, post
   under your name, and hear about new messages over a connection Ship of Tools opens itself (so nothing needs to reach
   your computer from outside).
3. **Install App** → **Install to Workspace** → **Allow**. Copy the **Bot User OAuth Token**; it starts with `xoxb-`.
4. **Basic Information** → **App-Level Tokens** → **Generate Token and Scopes**. Give it any name, add the scope
   `connections:write`, and **Generate**. Copy the token; it starts with `xapp-`.

Keep both tokens private, like passwords.

## 2. Find the IDs

Open Slack in a web browser and click the channel. The address looks like
`https://app.slack.com/client/T0123ABCD/C0456EFGH`:

- `T0123ABCD` is your **workspace ID**.
- `C0456EFGH` is the **channel ID**. (In the Slack app it is also at the bottom of the channel's details.)

## 3. Add the app to the channel

In the channel, type `/invite @Ship of Tools` (the name you gave the app). Slack lets the app read and post only in
channels it has been added to, private ones included.

## 4. Tell Ship of Tools

On the computer that runs your sessions, in Ship of Tools' settings folder (`~/.config/sot` on Linux and macOS,
`%LOCALAPPDATA%\sot\config` on Windows), make two files.

`chat-secrets.toml`, with your workspace ID and the two tokens:

```toml
workspace = "T0123ABCD"
bot_token = "xoxb-..."
app_token = "xapp-..."
```

On Linux or macOS, make it readable only by you: `chmod 600 ~/.config/sot/chat-secrets.toml`.

`chat.toml`, which says which channel goes with which project folder:

```toml
workspace = "T0123ABCD"
channels = ["C0456EFGH"]

[[mapping]]
folder = "/home/you/projects/my-project"
channel = "C0456EFGH"
```

`folder` is the project's folder as its session row shows it. On Windows, write the folder in single quotes: `folder = 'C:\Users\you\projects\my-project'`.

## 5. Restart and open it

Restart Ship of Tools' background service on that computer (on Linux: `systemctl --user restart sotd`). Your sessions
keep running; their Julia REPLs start fresh. Then select the project in the window and press `Ctrl+K` (`Cmd+K` on a Mac). The first time
you post, the window asks for the name to post under.

## Adding another project later

Find the channel's ID (step 2), add the app to the channel (step 3), then in `chat.toml` add the ID to `channels` and
add another `[[mapping]]` with the project's folder and that channel, and restart (step 5). One channel per project.

## If it does not show

The Chat drawer's title says what is wrong. `the row's folder maps to no channel` means the folder in `chat.toml` is
not exactly the row's folder. `the hub has no usable chat configuration` means a file was refused: the background
service's log says which file and which line (`sotd log-path` prints where the log is). A token or ID is never printed
in the log. The full rules for both files are in [the configuration reference](../ref/config.md#chat.toml-and-chat-secrets.toml).
