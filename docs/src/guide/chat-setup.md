# Setting up Slack for Ship of Tools

Ship of Tools can show a Slack channel next to each project, in the Chat drawer (`Ctrl+K`, `Cmd+K` on a Mac), and post
to it. You connect each Slack workspace you use once, from the drawer, then pick each project's channel there; no file is edited and nothing is
restarted. You need to be allowed to add apps to your Slack workspace, or an administrator who approves them.

The chat runs on one computer: the one that runs your project sessions. If you use Ship of Tools on one computer, that
is the one, with no `hosts.toml` needed. If you have several, it is the one your `hosts.toml` names as `hub`. Every
window shows the chat of that computer, and a project's chat is that of a session row on it.

**Several Slack workspaces.** The chat can connect up to 8 Slack workspaces, with your own Slack app in each, and each
project's channel is in one of them. To add a workspace, press `Ctrl+Shift+K` (`Cmd+Shift+K` on a Mac) in the Chat drawer, choose **Connect a
Slack workspace** at the end of the channel list, and go through the steps below with an app made in that workspace (all
three Slack codes come from that one app). Connecting another workspace adds it and leaves the others and their projects'
channels as they are. The channel list then shows one workspace at a time, named in its title; Left and Right move
between them. If one workspace's Slack codes stop working, the other workspaces' projects go on, and a project whose
channel is in the stopped workspace says which one; there, `Ctrl+Shift+K` (`Cmd+Shift+K` on a Mac) opens the channel list of the others, to move
the project to a channel in another workspace. An older window lists and picks channels of the first workspace only; an older
Ship of Tools on the computer that runs your agents connects one workspace, and whoever runs it there can update it.

## Once per Slack workspace: connect it

Each person makes their own Slack app in each workspace they connect, even when a colleague already has one there: Slack
hands each new message to one of an app's connections, so two people sharing one app's Slack codes each miss messages.

Open the Chat drawer on a project. While Slack is not connected, the drawer shows **set up Slack** and leads you
through three steps on Slack's website, one screen each; Left and Right move between the steps, PageUp and PageDown
(or the mouse wheel) scroll a screen taller than the drawer, which says `PageDown for more` above its keys, and Escape closes the drawer (opening it again
starts over at the first screen). Have Slack's website open and signed in, in your web browser (the Slack desktop app
is not enough).

1. **Make your Slack app.** Press Enter: your browser opens Slack's page that makes your app, already named
   `Ship of Tools (<the name you sign in to this computer with>)` (you can change it there; Slack shows that name on the
   messages your app posts). If Slack asks which workspace the app is for, pick the one you are connecting. The app can list your workspace's channels, read and post in the ones it is in, and, with your own
   Slack code, post your messages as you; it cannot read direct messages or channels it is not in. Click **Next**, then **Create**, then
   **Install to Workspace**, then **Allow**, and press Enter in the drawer. If Slack says the app needs an
   administrator's approval, send the request and wait for Slack's message that it is approved; then, on the app's page
   in Slack, click **Install to Workspace** and **Allow**. (If you closed the drawer meanwhile, open it and press Right
   on the first screen to go straight to step 2.) If Slack's page was empty, press `Ctrl+C` (`Cmd+C` on a Mac) on this
   step to copy the app's description, then at [api.slack.com/apps](https://api.slack.com/apps) click
   **Create New App**, **From a manifest**, choose the workspace you are connecting, paste it, **Next**, **Create**.
2. **The first two Slack codes.** Slack shows the app's **User OAuth Token**, which starts with `xoxp-` and lets Ship of
   Tools post your messages as you, and its **Bot User OAuth Token**, which starts with `xoxb-` (if not, at
   api.slack.com/apps click your app, then **OAuth & Permissions**). Click **Copy** beside one, click in the drawer and
   press `Ctrl+V` (`Cmd+V` on a Mac); then do the same with the other, in either order. If the page shows no User
   OAuth Token, your app was made before Ship of Tools posted as you: on that page, under **User Token Scopes**, click
   **Add an OAuth Scope** and choose `chat:write` (post as you), then again for `files:write` (post a file as you); then click **Reinstall to Workspace**
   and **Allow**, and the **User OAuth Token** shows at the top. (These names are Slack's own as its documentation gives them; if
   its page words them otherwise, follow its words, and the guide will be corrected.)
3. **The last Slack code.** Copy it from the same app as the two codes of step 2: all three codes come from one app. In
   Slack's menu on the left, click **Basic Information**; under **App-Level Tokens** click
   **Generate Token and Scopes**, type a name (`messages`), click **Add Scope**, choose `connections:write` (receive new messages as they arrive), click
   **Generate**, then **Copy**. Click in the drawer and press `Ctrl+V`.

A Slack code is a long string of letters and digits, like a password, that lets Ship of Tools act as your app: paste it only in the drawer. The
drawer shows a pasted code as its first five characters and its length, and a paste that is not a Slack code is
dropped; a paste in these steps never goes into a message. Once all three are pasted, the drawer checks them with Slack
(**connecting…**). Closing the drawer then does not stop the check: the hub finishes it, and a new try waits for its
answer. The drawer says **connected**, with the workspace's name; the hub keeps that workspace's three codes in its settings
folder, readable only by your login there, and Enter goes on to choosing the project's channel in that workspace. If Slack refuses a code, the
drawer goes back to its step and says so; the other codes are kept, so paste the refused one again.

If Slack later stops accepting the saved codes (the app was removed from the workspace, say), the drawer says so, even
while it shows a channel: the hub stops the chat, and the drawer shows its set-up screen in place of the channel. If the
App-Level Token the hub holds is another Slack app's (a colleague's, or your own older app's), the hub stops the chat
rather than take that app's messages, and the drawer says that the code belongs to another app. The connect cannot
tell whose App-Level Token it is: the hub finds out when it first listens, a moment after **connected**. On either screen,
press Enter to open your list of Slack apps: if your app is in it, click it and copy its three codes again (steps 2 and
3 follow in the drawer); if it is not, press Left to make the app again (step 1). A hub connected before Ship of Tools
posted your messages as you, or whose User OAuth Token Slack stopped accepting, refuses your post with that reason and shows the
same screen, titled `post as you`, in place of the channel: your draft is kept, and the codes are copied again as above.
Paste your own User OAuth Token only: the hub cannot tell whose it is, and would post as that person. To give a workspace new Slack codes at any
time, or to add one, choose **Connect a Slack workspace** (marked `add a Slack workspace, or give one new Slack codes`), last in the channel list (`Ctrl+Shift+K`).

## Per project: choose its channel

Select the project's row on the computer the chat runs on and open the Chat drawer (`Ctrl+K`). A project with no channel
yet shows the list once the drawer has the keys; press Escape to close it, and Enter later to open it again.

- Type to narrow the list (case does not matter, and a `#` is ignored), `Up` and `Down` to choose, `Enter` to pick, `Esc` to close.
  With several workspaces connected, the list shows one at a time, named in its title (` · Alpha Lab (1 of 2)`), and
  `Left` and `Right` move between them; it opens on the project's own workspace, or the one just connected.
- A channel your Slack app is not in is marked `your Slack app joins it when you pick it`: picking a public channel makes the app join
  it, and Slack then shows that your app joined. An app made by hand without the `channels:join` permission cannot join,
  and picking such a channel says so: invite the app to the channel (in it, type `/invite @` and your app's name, such as
  `/invite @Ship of Tools (lab-user)`).
- A private channel is in the list only once you have invited your app to it, and is marked `private; your Slack app is in it`.
- If your workspace has more channels than the list holds (1,000), the list says so: paste a channel's link or ID
  (`Ctrl+V`) to pick it. A channel's link is in its details in Slack; its last part is the ID, which begins with `C` or
  `G` and is made of capital letters and digits. The address in your browser's Slack tab
  (`https://app.slack.com/client/T.../C...`) also names the workspace, and is picked there, or refused when that workspace
  is not running here; the link Slack's Copy link gives (`https://<your workspace>.slack.com/archives/C...`) names it by its
  web address alone, so it is picked in the workspace the list shows.
- After you pick, the drawer shows the channel at once. Another project's channel cannot be picked again: it says that
  another project already uses it.

To change a project's channel, or to give it none, press `Ctrl+Shift+K` (`Cmd+Shift+K` on a Mac; `chat.setup`) in the Chat
drawer: the list then begins with `No channel for this project`, and ends with `Connect a Slack workspace`. The window writes
the choice into `chat.toml` for you, whole: comments and the order of its keys are not kept. One channel per project.

## Setting it up by hand

The drawer writes the same two files a person can write. To write them yourself instead:

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

   Copy the **User OAuth Token** (`xoxp-`) and the **Bot User OAuth Token** (`xoxb-`), and make an App-Level Token with the scope `connections:write` as in
   step 3 above (`xapp-`).
2. Open Slack in a web browser and click the channel. The address looks like
   `https://app.slack.com/client/T0123ABCD/C0456EFGH`: `T0123ABCD` is your workspace's ID and `C0456EFGH` the
   channel's. Invite your app to the channel.
3. In the hub's settings folder (`~/.config/sot` on Linux and macOS, `%LOCALAPPDATA%\sot\config` on Windows), make
   `chat-secrets.toml`, readable only by you (on Linux or macOS, `chmod 600 ~/.config/sot/chat-secrets.toml`):

   ```toml
   workspace = "T0123ABCD"
   bot_token = "xoxb-..."
   user_token = "xoxp-..."
   app_token = "xapp-..."
   ```

   and `chat.toml`, which says which channel goes with which project folder (the folder as its session row shows it; on
   Windows in single quotes, `folder = 'C:\Users\you\projects\my-project'`):

   ```toml
   workspace = "T0123ABCD"
   channels = ["C0456EFGH"]

   [[mapping]]
   folder = "/home/you/projects/my-project"
   channel = "C0456EFGH"
   ```

   For a project whose channel is in another workspace, make that workspace's file beside it, named for its ID
   (`chat-secrets-T0987WXYZ.toml`), in the same four lines with that workspace's ID and the three Slack codes of the app made
   there, and give the project's mapping that workspace; its channel goes in the same `channels` list:

   ```toml
   [[mapping]]
   folder = "/home/you/projects/other-project"
   workspace = "T0987WXYZ"
   channel = "C0789IJKL"
   ```
4. Restart Ship of Tools' background service on the hub (on Linux: `systemctl --user restart sotd`). Your sessions
   keep running; their Julia REPLs start fresh. A file edited by hand is read at the service's start; a change from the
   window applies at once.

## If it does not show

The Chat drawer's title says what is wrong. `no channel` means no channel is set for this project: pick one as above, or,
if you wrote `chat.toml` by hand, the folder in it is not exactly the row's folder. `the chat on <computer> is not set up correctly` means a file was refused: the log of Ship of Tools' service on that
computer says which file and which line (the title says to run `sotd log-path` in a terminal on that computer, which prints where the log is). A Slack code or ID is never printed
in the log. The full rules for both files are in [the configuration reference](../ref/config.md#chat.toml-and-chat-secrets.toml).
