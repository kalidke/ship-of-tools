# Setting up Slack for Ship of Tools

Ship of Tools can show a Slack channel beside each project, in the Chat panel, and post to it. This page takes you
through it once, with the window open beside it: connect a Slack workspace, pick a project's channel, connect a second
workspace, and get a private channel into the list. Done from the panel, nothing is edited by hand and nothing is
restarted; [Setting it up by hand](#Setting-it-up-by-hand) gives the files for those who would rather write them. How to
use the panel afterward is in [The project chat](chat.md). [The interface](interface.md) shows where the panels, the
navigation tree and its status line sit in the window; a session is an agent running in one project's folder
([Your first session](../start/tour.md)).

Keys are written as on Linux and Windows; on a Mac, press Cmd where this page says Ctrl. A letter in a key is always
lowercase: `Ctrl+k` is Ctrl and k, and `Ctrl+Shift+k` adds Shift. In the window, `F1` opens Help, and Enter on its
`Choose channel` action (`Ctrl+Shift+k`) opens this page in your browser.

## Before you start

- **Slack.** You need Slack's website open and signed in, in your web browser (the Slack desktop app is not enough),
  and permission to add apps to your workspace, or an administrator who approves them.
- **The computer that runs your sessions.** The chat runs on one computer: the one that runs your project sessions. If
  you use Ship of Tools on one computer, that is the one. If you use several, it is the one your `hosts.toml` names on
  its `hub =` line; a project whose session runs on another computer shows no chat (its panel says
  `the chat does not run on <computer>`). Every window shows the chat of that computer.
- **The Chat panel.** It is the bottom drawer's Chat view. Select the project's session and press `Ctrl+k`: the panel
  opens with the keys, and its top line reads `chat ·` and what it shows. `Ctrl+k` again hides it.
- **Your own Slack app.** You make one Slack app for yourself in each workspace you connect, even when a colleague
  already has one there: Slack hands each new message to one connection of an app, so two people sharing one app each
  miss messages.

## Connect your first Slack workspace

While Slack is not connected, the panel's top line reads `chat · set up Slack` and the panel leads you through three
steps on Slack's website, one screen each. On every screen its keys are listed at its foot. Left and Right move between
the steps and keep what you pasted. A screen taller than the panel says `PageDown for more`; PageUp, PageDown and the mouse wheel scroll it.
Escape closes the panel and drops the codes pasted so far; you start again at the first screen.

### The first screen

It says what the panel is for and what the three steps do. Nothing opens yet. **Press Enter:** your browser then opens
Slack's page that makes your app, and the panel shows step 1. If the browser cannot be opened, the status line above the
navigation tree says `could not open the browser` and why; go to [api.slack.com/apps](https://api.slack.com/apps)
yourself.

If you already made your Slack app in this workspace, press Right instead ("I already made my Slack app: go to step 2").

### Step 1: make your Slack app

Slack's page has the app filled in, named `Ship of Tools (<your login>)`: your login is the name you sign in to the
computer the window runs on with, so a person who signs in as `lab-user` gets `Ship of Tools (lab-user)`. Slack shows
that name on what your app posts, and you invite the app to a channel by it. You can change it on Slack's page; the
name holds at most 35 characters, so a long login is cut, and with no login it is `Ship of Tools`.

1. If Slack asks which workspace the app is for, pick the one you are connecting.
2. Click **Next**, then **Create**.
3. Click **Install to Workspace**, then **Allow**.
4. Press Enter in the panel: it shows step 2.

The app can list your workspace's channels, read and post in the ones it is in, and, with your own Slack code, post
your messages as you. It cannot read direct messages or channels it is not in.

If Slack says the app needs an administrator's approval, send the request and wait for Slack's message that it is
approved; then, on the app's page in Slack, click **Install to Workspace** and **Allow**. If you closed the panel
meanwhile, open it and press Right on the first screen to go straight to step 2.

If Slack's page came up empty, press `Ctrl+c` in the panel on this step: it copies the app's description (Slack calls
it a manifest). At [api.slack.com/apps](https://api.slack.com/apps) click **Create New App**, then **From a manifest**,
choose the workspace you are connecting, paste it, then **Next** and **Create**, and go on at item 3 above.

### Step 2: the first two Slack codes

A Slack code (Slack calls it a token) is a long string of letters and digits, like a password, that lets Ship of Tools
act as your app: paste it only in the panel. Slack now shows two of them:

- the **User OAuth Token**, which starts with `xoxp-` and lets Ship of Tools post your messages as you;
- the **Bot User OAuth Token**, which starts with `xoxb-` and lets your app read your channels and post in them.

If they are not on screen, at [api.slack.com/apps](https://api.slack.com/apps) click your app, then **OAuth &
Permissions** in the menu on the left. Click **Copy** beside one, click in the panel and press `Ctrl+v`; then do the
same with the other, in either order. The panel shows each code as its first five characters and its length
(`pasted: xoxb-…, 57 characters`), and goes on to step 3 once both are pasted. A paste that is not a Slack code is
dropped, and the status line says `that is not a Slack code` and how each of the three starts.

If the page shows no User OAuth Token, your app was made before Ship of Tools posted as you. On that page, under **User
Token Scopes**, click **Add an OAuth Scope** and choose `chat:write` (post as you), then again for `files:write` (post a
file as you). Then click **Reinstall to Workspace** and **Allow**; the User OAuth Token shows at the top.

These names are Slack's own, as its documentation gives them; if its page words them otherwise, follow its words.

### Step 3: the last Slack code

The **App-Level Token** lets Ship of Tools see new messages as they arrive. Copy it from the same app as the two codes of
step 2: all three come from one app.

1. In Slack's menu on the left, click **Basic Information**.
2. Under **App-Level Tokens**, click **Generate Token and Scopes**.
3. Type a name, such as `messages`.
4. Click **Add Scope** and choose `connections:write`.
5. Click **Generate**, then **Copy**.
6. Click in the panel and press `Ctrl+v`.

### Connected

With all three codes pasted, the panel sends them to the computer that runs your sessions, which checks them with Slack
(`connecting…`). Closing the panel does not stop the check. When Slack accepts them, the top line reads `connected`
and the screen names the workspace. That computer keeps the workspace's three codes in its settings folder
(`~/.config/sot` on Linux and macOS, `%LOCALAPPDATA%\sot\config` on Windows), readable only by your login there, and
the panel forgets them.

If Slack refuses a code, the panel goes back to its step and says which one; the other codes are kept, so paste only
that one again. Any other failure brings back the first screen, which then begins `Not connected:` and the reason.

For a few seconds the connected screen says it is checking which of the projects' channels your app can read. Then
it offers to choose this project's channel: press Enter. If it cannot tell within about 12 seconds, it says so: a
project whose panel then shows nothing needs your app invited to its channel (see below).

## Choose a project's channel

The channel list opens when you press Enter on the connected screen, and by itself, once, when you open the Chat panel
on a project with no channel. Its top line reads `chat · choose this project's channel` and the workspace's name.

- Type to narrow the list (case does not matter, and a `#` is ignored). Up and Down choose, Enter picks, Escape closes.
- After Escape the panel says `this project has no channel: press Enter to pick one`, and Enter opens the list again.
- A public channel your app is not in is marked `your Slack app joins it when you pick it`: picking it makes the app
  join, and Slack shows that it joined.
- A channel another project already uses cannot be picked: the status line says `not set: another project already uses
  that channel`. Each project has one channel.

After you pick, the panel shows the channel's messages.

### A private channel

Slack lists a private channel only to a Slack app that is a member of it, so a private channel is missing from the
list until someone in it invites your app. To get it into the list:

1. In Slack, open the private channel.
2. In its message box, type `/invite @` and your app's name, such as `/invite @Ship of Tools (lab-user)`, and send it.
   You must be a member of the channel; otherwise ask a member to do it.
3. In the Chat panel, press Escape to close the list, then `Ctrl+Shift+k` to open it again.

The channel is then in the list, marked `private; your Slack app is in it`. The list says the same above its entries.

### A channel the list does not show

The list holds 1,000 channels; for a workspace with more it says `More channels exist than are listed: paste a
channel's link to pick one.` Paste a channel's link or ID with `Ctrl+v` to pick it.
A channel's link is in its details in Slack; its last part is the channel's ID, which begins with `C` or `G` and is made
of capital letters and digits. The address in your browser's Slack tab (`https://app.slack.com/client/T.../C...`) also
names the workspace, and is picked in that workspace, or refused when it is not connected here; the link Slack's
**Copy link** gives names no workspace, so it is picked in the workspace the list shows.

### Change or remove a project's channel

Select the project's session, open the Chat panel and press `Ctrl+Shift+k`. The list then begins with
`No channel for this project` and ends with `Connect a Slack workspace`. Your choice is written into `chat.toml`, the
chat's settings file on the computer that runs your sessions, whole: comments and the order of its keys are not kept.

## Connect another Slack workspace

One computer can connect up to 8 Slack workspaces, each with your own app made there, and each project's channel can
be in any of them: one workspace per project, if you want. Connecting another workspace leaves the ones already
connected, and their projects' channels, as they are. A ninth is refused, and the panel says which file to remove to
make room.

1. In the Chat panel of any project, press `Ctrl+Shift+k`.
2. With nothing typed, choose `Connect a Slack workspace`, the last entry (marked `add a Slack workspace, or give one
   new Slack codes`), and press Enter.
3. Follow the steps above. In step 1, pick the new workspace on Slack's page, so the new app is made there.
4. On the connected screen, Enter opens the new workspace's channels for the project you started from. Pick one to
   move that project there, or press Escape to leave its channel as it is.

With more than one workspace connected, the panel's top line names the workspace before the channel
(`chat · Alpha Lab · #lab-general`).

### Pick a channel in another workspace

Nothing needs to be set up again. Select the project's session and open the Chat panel; the list opens by itself if
the project has no channel, and `Ctrl+Shift+k` opens it otherwise. The list shows one workspace at a time, named in its
top line with its place (`chat · choose this project's channel · Alpha Lab (1 of 2)`). Press Right or Left to show the
next or previous workspace's channels, then pick as before. The list opens on the workspace of the project's channel;
for a project with no channel, on the workspace connected first; and after you connect one, on that one.

## Give a connected workspace new Slack codes

You give a workspace new codes when the panel asks for them (the `post as you` screen, or the screen that says Slack no
longer accepts the codes), or when you choose to. **Keep the Slack app that workspace is connected to now; do not make
a new one.** That app is in every channel your projects use; a new app is in none of them.

### The `post as you` screen

It shows in place of the channel when your post is refused because the computer that runs your sessions holds no code
that posts as you: it was connected before Ship of Tools posted as you, or Slack stopped accepting that code. Your
message is kept.

When that computer has said which app the workspace is connected to, the screen says first to keep that app, and names
it as Slack calls it. Enter is meant to open that app's own page in Slack, and the panel shows
step 2; if your browser shows your list of Slack apps instead, click the app the screen names. Before that computer has
answered, or from an older Ship of Tools, Enter opens your list of Slack apps: click the app you connected this
workspace with before, whatever its name.

Then, on the app's page:

1. Under **User Token Scopes**, add `chat:write` and `files:write`, as step 2 describes, if they are not there.
2. Click **Reinstall to Workspace**, then **Allow**.
3. Paste the User OAuth Token and the Bot User OAuth Token (step 2), then an App-Level Token of the same app (step 3).

Press Left on the `post as you` screen to make a new app only if the old one is gone (it is not in your list of Slack
apps).

### At any other time

Press `Ctrl+Shift+k`, choose `Connect a Slack workspace`, and on the first screen press **Right** ("I already made my
Slack app: go to step 2"), not Enter: Enter opens Slack's page that makes a new app. At
[api.slack.com/apps](https://api.slack.com/apps) click the app this workspace is connected to now, and copy its codes as
steps 2 and 3 describe.

### If you connected a new app

Connecting a new app replaces the workspace's old codes without asking first. A new app is in no channel, public or private,
until it is invited, so each project whose channel it is not in shows nothing. A few seconds after the connect, the
connected screen names each such channel and what to type in it. Read it before pressing Enter: Enter opens the
channel list, and choosing another channel there gives up the one the project had. To recover:

- In each channel it names, type `/invite @` and the new app's name in Slack's message box, and send it. The panel shows
  the channel within two minutes after.
- For a public channel, picking the same channel again in the list (`Ctrl+Shift+k`) also works: the app joins it.
- Do not choose another channel in the list instead.

A project whose channel the app is not in says `your Slack app cannot read this channel` in its top line, and the panel
says what to type, until you invite the app.

## When Slack stops accepting the codes

If Slack stops accepting the saved codes (the app was removed from the workspace, say), the panel shows a set-up screen
in place of the channel, saying so. If the App-Level Token held is another Slack app's (a colleague's, or one of your
older apps), the chat stops rather than take that app's messages, and the screen says the code belongs to another app;
the connect cannot tell whose App-Level Token it is, so this shows a moment after `connected`. On either screen, Enter
opens your list of Slack apps and shows step 2: click your app and copy its three codes again. If your app is not in
the list, press Left to make it again (step 1), and then invite it to your projects' private channels.

The other workspaces go on. A project whose channel is in the stopped workspace names that workspace on its first
screen; there, while another workspace runs, `Ctrl+Shift+k` opens that workspace's channel list, to move the project to
a channel in it.

An older window lists and picks channels of the first workspace only. An older Ship of Tools on the computer that runs
your sessions connects one workspace, and its list says so; whoever runs it there can update it.

## Setting it up by hand

Connecting from the panel writes the same two files a person can write. To write them yourself instead:

1. At [api.slack.com/apps](https://api.slack.com/apps), choose **Create New App**, then **From a manifest**, pick your
   workspace, paste this (with your own name in the two names; Slack takes at most 35 characters), then **Create**,
   **Install to Workspace**, **Allow**:

   ```yaml
   display_information:
     name: Ship of Tools (lab-user)
   features:
     bot_user:
       display_name: Ship of Tools (lab-user)
       always_online: true
   oauth_config:
     scopes:
       user:
         - chat:write
         - files:write
       bot:
         - channels:history
         - channels:read
         - channels:join
         - groups:history
         - groups:read
         - users:read
         - chat:write
         - chat:write.customize
         - files:read
         - files:write
   settings:
     event_subscriptions:
       bot_events:
         - message.channels
         - message.groups
     org_deploy_enabled: false
     socket_mode_enabled: true
     token_rotation_enabled: false
   ```

   Copy the User OAuth Token (`xoxp-`) and the Bot User OAuth Token (`xoxb-`), and make an App-Level Token with the
   scope `connections:write` as in step 3 above (`xapp-`).
2. Open Slack in a web browser and click the channel. The address looks like
   `https://app.slack.com/client/T0123ABCD/C0456EFGH`: `T0123ABCD` is your workspace's ID and `C0456EFGH` the
   channel's. Invite your app to the channel.
3. In the settings folder of the computer that runs your sessions (`~/.config/sot` on Linux and macOS,
   `%LOCALAPPDATA%\sot\config` on Windows), make `chat-secrets.toml`, readable only by you (on Linux or macOS,
   `chmod 600 ~/.config/sot/chat-secrets.toml`):

   ```toml
   workspace = "T0123ABCD"
   bot_token = "xoxb-..."
   user_token = "xoxp-..."
   app_token = "xapp-..."
   ```

   and `chat.toml`, which says which channel goes with which project folder (the folder as its session shows it; on
   Windows in single quotes, `folder = 'C:\Users\you\projects\my-project'`):

   ```toml
   workspace = "T0123ABCD"
   channels = ["C0456EFGH"]

   [[mapping]]
   folder = "/home/you/projects/my-project"
   channel = "C0456EFGH"
   ```

   For a project whose channel is in another workspace, make that workspace's file beside it, named for its ID
   (`chat-secrets-T0987WXYZ.toml`), in the same four lines with that workspace's ID and the three Slack codes of the app
   made there, and give the project's mapping that workspace; its channel goes in the same `channels` list:

   ```toml
   [[mapping]]
   folder = "/home/you/projects/other-project"
   workspace = "T0987WXYZ"
   channel = "C0789IJKL"
   ```
4. Restart Ship of Tools' background service on that computer (on Linux: `systemctl --user restart sotd`). Your
   sessions keep running; their Julia REPLs start fresh. A file edited by hand is read at the service's start; a change
   from the window applies at once.

The full rules for both files are in [the configuration reference](../ref/config.md#chat.toml-and-chat-secrets.toml).

## If the panel shows no channel

The panel's top line says what is wrong; [The project chat](chat.md#What-the-top-line-says) lists every case. The ones
met while setting up:

- `no channel`: the project has no channel yet. Press Enter in the panel to pick one. If you wrote `chat.toml` by hand,
  its `folder` is not exactly the project's folder.
- `your Slack app cannot read this channel`: invite your app to the channel, as above.
- `the chat on <computer> is not set up correctly`: a file was refused. The log of Ship of Tools' service on that
  computer says which file and which line; run `sotd log-path` in a terminal there to find the log. A Slack code or ID
  is never printed in the log.
- `the chat does not run on <computer>`: the project's session runs on a computer other than the one named on the
  `hub =` line of `hosts.toml`.
