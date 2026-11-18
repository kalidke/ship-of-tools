# Setting up Slack for Ship of Tools

Ship of Tools can show a Slack channel beside each project, in the Chat panel, and post to it. This page takes you
through the setup once, with the window open beside it: connect a Slack workspace, pick a project's channel, connect a
second workspace, and get a private channel into the list. The panel does all of it and writes the settings files for
you; [Setting it up by hand](#Setting-it-up-by-hand), at the end, writes the same files yourself instead. How to use the
panel afterward is in [The project chat](chat.md).

Keys are written as on Linux and Windows. On a Mac, press Cmd where this page says Ctrl (`Ctrl+Shift+k` is
Cmd+Shift+k); the window's own key lists show the Mac's symbols, such as `⇧⌘k` (⇧ is Shift, ⌘ is Cmd), and the setup
screens write `Cmd`. A letter in a key is always lowercase: `Ctrl+k` is Ctrl and k, and `Ctrl+Shift+k` adds Shift.

## Before you start

- **Sessions.** A session is one coding agent, such as Claude Code or Codex, running in one project's folder. The
  navigation tree, the window's left column, has modes: press `s` there for Sessions mode, which lists your sessions,
  and `f` for Files mode, which lists the project's files. Select the project's session in Sessions mode, or move
  between sessions with `Shift+ArrowLeft` and `Shift+ArrowRight`.
- **The Chat panel.** The bottom of the window shows one thing at a time: the Julia REPL, a terminal, the monitor, Help
  or the Chat panel. `Ctrl+k` shows the Chat panel there and gives it the keys; `Ctrl+k` again hides it. Its top line
  reads `chat ·` and what it shows. What a key did is said on the status line, the line starting `status:` at the top
  of the navigation tree.
- **The computer that runs your sessions.** On each of your computers, Ship of Tools' service (`sotd`, a background
  program apart from the window) runs the sessions there. The chat runs on one of them, which this
  page calls the computer that runs your sessions: with one computer, it is that one; with several, it is the one your
  `hosts.toml` (the file that lists your computers, in `~/.config/sot` on Linux and macOS and `%LOCALAPPDATA%\sot\config`
  on Windows) names in its line `hub = "<computer>"`. A project whose session runs on another computer has no chat: its
  panel says `the chat does not run on <computer>`. The panel's messages name a computer by its name, `this computer`
  for the one the window runs on, or `the computer that runs your sessions`.
- **Slack.** You need Slack's website open and signed in, in your web browser (the Slack desktop app is not enough),
  and permission to add apps to your workspace, or an administrator who approves them.
- **Your Slack app.** In each workspace you make a Slack app for yourself. Your Slack app reads the project's channel
  and posts your agents' messages; what you write in the panel goes out under your own Slack account. In Slack the app
  is a member of its own, named like the app, which a channel invites as it would a person. Each person makes their
  own, even when a colleague already has one there: by Slack's rule each new message goes to only one of an app's
  connections, so two people sharing one app each miss messages.
- **Slack codes.** Three Slack codes (Slack calls them tokens) let Ship of Tools use your Slack app and your account:
  the **User OAuth Token**, which starts with `xoxp-` and posts your messages as you; the **Bot User OAuth Token**,
  which starts with `xoxb-` and lets your Slack app read and post; and the **App-Level Token**, which starts with
  `xapp-` and lets your Slack app receive new messages as they arrive. A Slack code is like a password: paste it only
  in the panel.

## Connect your first Slack workspace

Select a project's session and press `Ctrl+k`. While no Slack workspace is connected, the panel shows the first screen,
then step 1, step 2 and step 3, which you follow on Slack's website, then the connected screen. Two other screens come
only when something needs fixing, each in its own section below: the first screen as it shows after Slack refused the
saved codes ([When Slack stops accepting the codes](#When-Slack-stops-accepting-the-codes)), and the post-as-you screen,
which asks for new codes after a post of yours was refused ([The post-as-you screen](#The-post-as-you-screen)). Each
screen lists its keys at its foot. A screen taller than the panel says `PageDown for more`; PageUp, PageDown and the
mouse wheel scroll it.

The panel holds the codes you paste until it has all three, then sends them. Closing the panel before that (Escape,
`Ctrl+k`, or selecting another session) drops every code pasted so far, and you start again at the first screen. Once
the three are sent, closing the panel does not stop the check.

### The keys on each screen

On each of these screens Escape closes the panel (in the channel list it closes only the list), and `Ctrl+v` pastes a
Slack code until the codes are sent.

| Screen (its top line) | Enter | Right | Left |
|---|---|---|---|
| The first screen (`chat · set up Slack`) | Opens Slack's page that makes your Slack app in your browser, and shows step 1. | Shows step 2, opening nothing: for a Slack app you already made. | Nothing. |
| The first screen when Slack refused the saved codes (`chat · set up Slack`), and the post-as-you screen (`chat · post as you`) | Opens your Slack app's page, or your list of Slack apps, and shows step 2. | Nothing. | Opens Slack's page that makes a new Slack app, and shows step 1. |
| Step 1 (`step 1 of 3: make your Slack app`) | Shows step 2. | Shows step 2. | Shows the first screen. |
| Step 2 (`step 2 of 3: the first two Slack codes`) | Nothing, unless the status line said to wait: then it sends the codes (see below). | Shows step 3. | Shows step 1. |
| Step 3 (`step 3 of 3: the last Slack code`) | The same as on step 2. | Nothing. | Shows step 2. |
| `connecting…` | Nothing. | Nothing. | Nothing. |
| The different Slack app's screen (`chat · a different Slack app`), when it says the codes belong to a different Slack app | Keeps the Slack app the workspace is connected to now: opens that app's page in your browser and shows step 2, with the codes you pasted dropped. | Connects the codes you pasted anyway, in place of the Slack app connected now (the screen names this key `→ (Right)`). | Nothing. |
| The same screen, when it says Ship of Tools could not tell whose codes these are | Asks Slack again, with the same three codes; nothing is changed unless Slack then says they belong to the app connected now. | Connects the codes you pasted anyway (the screen names this key `→ (Right)`). | Nothing. |
| The connected screen (`chat · connected`) | Opens the channel list. | Nothing. | Nothing. |

The three codes are sent by themselves when the last one is pasted. Only if an earlier check is still running does the
status line say `Slack is still checking the codes sent before`; then wait, and press Enter to send them.

### The first screen

It says what the panel is for. Nothing opens until you press Enter: then your browser opens Slack's page that makes
your Slack app, and the panel shows step 1. If the browser cannot be opened, the status line says
`could not open the browser` and why; go to [api.slack.com/apps](https://api.slack.com/apps) yourself. If you already
made your Slack app in this workspace, press Right instead. (After Slack refused the saved codes, the first screen says
so and its keys differ: see [When Slack stops accepting the codes](#When-Slack-stops-accepting-the-codes).)

### Step 1: make your Slack app

Slack's page has your Slack app filled in, named `Ship of Tools (<your login>)`. Your login is the name you sign in to
the window's computer with, even when your sessions run on another, so a person who signs in as `lab-user` gets
`Ship of Tools (lab-user)`. Slack shows that name on what your Slack app posts, and you invite the app to a channel by
it. You can change it on Slack's page. Slack takes at most 35 characters for the name, so a long login is cut short,
and with no login the name is `Ship of Tools`.

1. If Slack asks which workspace the app is for, pick the one you are connecting.
2. Click **Next**, then **Create**.
3. Click **Install to Workspace**, then **Allow**.
4. Press Enter in the panel: it shows step 2.

Your Slack app can list your workspace's channels, read and post in the ones it is in, and, with the User OAuth Token,
post your messages as you. It cannot read direct messages or channels it is not in.

If Slack says the app needs an administrator's approval, send the request and wait for Slack's message that it is
approved; then, on the app's page in Slack, click **Install to Workspace** and **Allow**. If you closed the panel
meanwhile, open it and press Right on the first screen to go straight to step 2.

If Slack's page came up empty, press `Ctrl+c` in the panel on step 1: it copies the app's description (Slack calls it a
manifest). At [api.slack.com/apps](https://api.slack.com/apps) click **Create New App**, then **From a manifest**, choose
the workspace you are connecting, paste it, then **Next** and **Create**, and go on at item 3 above.

### Step 2: the first two Slack codes

Slack now shows the User OAuth Token (`xoxp-`) and the Bot User OAuth Token (`xoxb-`). If they are not on screen, at
[api.slack.com/apps](https://api.slack.com/apps) click your Slack app, then **OAuth & Permissions** in the menu on the
left. Click **Copy** beside one, click in the panel and press `Ctrl+v`; then do the same with the other, in either
order. The panel shows each code as its first five characters and its length (`pasted: xoxb-…, 57 characters`), and
shows step 3 once both are pasted. A paste that is not a Slack code is dropped, and the status line says
`that is not a Slack code` and how each of the three starts.

If the page shows no User OAuth Token, your Slack app was made before Ship of Tools posted as you. On that page, under
**User Token Scopes**, click **Add an OAuth Scope** and choose `chat:write` (post as you), then again for `files:write`
(post a file as you). Then click **Reinstall to Workspace** and **Allow**; the User OAuth Token shows at the top.

### Step 3: the last Slack code

Copy the App-Level Token from the same Slack app as the two codes of step 2: all three come from one app.

1. In Slack's menu on the left, click **Basic Information**.
2. Under **App-Level Tokens**, click **Generate Token and Scopes**.
3. Type a name, such as `messages`.
4. Click **Add Scope** (a button on this page, not step 2's Add an OAuth Scope) and choose `connections:write`.
5. Click **Generate**, then **Copy**.
6. Click in the panel and press `Ctrl+v`.

### The connected screen

With the three codes pasted, the panel sends them to the computer that runs your sessions, which checks them with
Slack; the top line reads `connecting…`. When Slack accepts them, the connected screen names the workspace. That
computer keeps the three codes in its settings folder (`~/.config/sot` on Linux and macOS, `%LOCALAPPDATA%\sot\config`
on Windows), readable only by your login there, and the panel forgets them.

If Slack refuses a code, the panel goes back to that code's step and says which code it was; the other two stay held,
so paste only that one again. Any other failure brings back the first screen, which then begins `Not connected:` and the
reason.

For a few seconds the connected screen says it is checking which of the projects' channels your Slack app can read.
Then it offers to choose this project's channel: press Enter. It stops checking after about 12 seconds and then says it
could not tell; a project whose panel then shows nothing needs your Slack app invited to its channel, as
[A private channel](#A-private-channel) shows (a public one is joined by picking it in the channel list).

## Choose a project's channel

Each project has one channel, and each channel serves one project. The channel list opens when you press Enter on the
connected screen, and by itself when you open the Chat panel on a project with no channel; after you close it with
Escape, it waits for Enter until you select another session. Its top line reads
`chat · choose this project's channel` and the workspace's name.

- Type to narrow the list (case does not matter, and a `#` is ignored). Up and Down choose, Enter picks, Escape closes.
- After Escape the panel says `this project has no channel: press Enter to pick one`, and Enter opens the list again.
- A public channel your Slack app is not in is marked `your Slack app joins it when you pick it`: picking it makes the
  app join, and Slack shows that it joined.
- A channel another project already uses cannot be picked: the status line says `not set: another project already uses
  that channel`.

After you pick, the panel shows the channel's messages.

### A private channel

Slack lists a private channel only to a Slack app that is a member of it, so a private channel is missing from the
list until someone in it invites your Slack app. To get it into the list:

1. In Slack, open the private channel. You must be a member of it; otherwise ask a member to do this.
2. In its message box, type `/invite @` and your Slack app's name, such as `/invite @Ship of Tools (lab-user)`, and
   send it. Slack offers matching names as you type; pick your app's.
3. In the Chat panel, press Escape to close the list, then `Ctrl+Shift+k` to open it again.

The channel is then in the list, marked `private; your Slack app is in it`. The list says the same above its entries.

### A channel the list does not show

The list holds 1,000 channels, a fixed limit of Ship of Tools; for a workspace with more it says `More channels exist than are
listed: paste a channel's link to pick one.` Paste a channel's link or ID with `Ctrl+v` to pick it. A channel's link is
in its details in Slack; its last part is the channel's ID, which begins with `C` or `G` and is made of capital letters
and digits. The address in your browser's Slack tab (`https://app.slack.com/client/T.../C...`) also names the workspace,
and is picked in that workspace, or refused when it is not connected here; the link Slack's **Copy link** gives names no
workspace, so it is picked in the workspace the list shows.

### Change or remove a project's channel

Select the project's session, open the Chat panel and press `Ctrl+Shift+k`. The list then begins with
`No channel for this project` and ends with `Connect a Slack workspace`. Your choice is written into `chat.toml`, the
chat's settings file on the computer that runs your sessions. If you edited that file by hand, a pick from the panel
rewrites it whole: its comments and the order of its keys are not kept.

## Connect another Slack workspace

The computer that runs your sessions can connect up to 8 Slack workspaces, a fixed limit of Ship of Tools, each with
your own Slack app made there, and each project's channel can be in any of them: one workspace per project, if you
want. Connecting another workspace leaves the ones already connected, and their projects' channels, as they are. A
ninth is refused, and the first screen says to remove one workspace's file, `chat-secrets-<id>.toml` (`<id>` is that
workspace's Slack ID), from the settings folder to make room.

1. In the Chat panel of any project, press `Ctrl+Shift+k`.
2. With nothing typed, choose `Connect a Slack workspace`, the last entry (marked `add a Slack workspace, or give one
   new Slack codes`), and press Enter: the first screen shows.
3. Follow the steps above. In step 1, pick the new workspace on Slack's page, so the new Slack app is made there.
4. On the connected screen, Enter opens the new workspace's channels for the project you started from. Pick one to
   move that project there, or press Escape to leave its channel as it is.

With more than one workspace connected, the panel's top line names the workspace before the channel
(`chat · Alpha Lab · #lab-general`).

### Pick a channel in another workspace

Nothing needs to be set up again. Select the project's session and open the Chat panel; the channel list opens by
itself if the project has no channel, and `Ctrl+Shift+k` opens it otherwise. The list shows one workspace at a time,
named in its top line with its place (`chat · choose this project's channel · Alpha Lab (1 of 2)`). Press Right or Left
to show the next or previous workspace's channels, then pick as before. The list opens on the workspace of the
project's channel; for a project with no channel, on the workspace connected first; and after you connect one, on that
one.

## Give a connected workspace new Slack codes

You give a workspace new codes when the panel asks for them (the post-as-you screen, or the first screen saying Slack
no longer accepts the codes), or when you choose to. **Keep the Slack app that workspace is connected to now; do not
make a new one.** That app is in every channel your projects use; a new app is in none of them.

### The post-as-you screen

It shows in place of the channel when your post is refused because the computer that runs your sessions holds no code
that posts as you: it was connected before Ship of Tools posted as you, or Slack stopped accepting that code. Your
message is kept.

A moment after it opens, the screen names the Slack app this workspace is connected to now, as Slack calls it, and
says to keep it; Enter then opens that app's page in Slack and shows step 2. If your browser shows your list of Slack
apps instead, click the app the screen names. When the screen does not say which app to keep (an older Ship of Tools
on the computer that runs your sessions does not tell it), Enter opens your list of Slack apps: click the app you
connected this workspace with before, whatever its name.

Then, on the app's page:

1. Under **User Token Scopes**, add `chat:write` and `files:write`, as step 2 describes, if they are not there.
2. Click **Reinstall to Workspace**, then **Allow**.
3. Paste the User OAuth Token and the Bot User OAuth Token (step 2), then an App-Level Token of the same app (step 3).

Press Left on the post-as-you screen to make a new app only if the old one is gone (it is not in your list of Slack
apps).

### At any other time

Press `Ctrl+Shift+k`, choose `Connect a Slack workspace`, and on the first screen press **Right**, not Enter: Enter opens
Slack's page that makes a new app. At [api.slack.com/apps](https://api.slack.com/apps) click the app this workspace is
connected to now, and copy its codes as steps 2 and 3 describe.

### If you paste the codes of a new app

When the codes you paste belong to a different Slack app than the one the workspace is connected to now, or Slack does
not say which app they belong to, nothing is changed yet: the panel asks first. Its top line reads
`chat · a different Slack app`, and it names the app connected now as Slack calls it. It has two forms.

**The codes belong to a different app.** The screen says so, and that a new app is in no channel yet, so each project's
channel would show nothing until someone invites the new app in it in Slack.

- **Enter keeps everything as it is.** Your browser opens the page of the app connected now, the panel shows step 2,
  and the codes you pasted are dropped: paste that app's three codes (steps 2 and 3).
- **Right (`→`) connects the codes you pasted anyway**, in place of the app connected now. The connected screen follows
  (below).

**Ship of Tools could not tell whose codes these are.** Slack did not answer when it was asked which app the codes
belong to, or which app the workspace is connected to.

- **Enter asks Slack again**, with the same three codes. If Slack then says they belong to the app connected now, they
  are saved; if it says they belong to a different app, the screen above shows; if it still does not answer, this
  screen shows again.
- **Right (`→`) connects the codes you pasted anyway.** If they belong to a new app, it is in no channel yet; if the
  three are not from one app, the chat stops a moment later (below).

### If the three codes are not from one app

All three codes must come from one Slack app. If the App-Level Token of step 3 belongs to a different Slack app than
the Bot User OAuth Token of step 2 (for example, the last code you copied from a new app was still on the clipboard
when you pasted the old app's two), nothing is saved and the workspace keeps its connection. The panel shows step 3
again with the App-Level Token dropped and the other two kept, and says first:
`Not connected: the "App-Level Token" belongs to a different Slack app than the "Bot User OAuth Token", so nothing was
changed; paste the three codes of one Slack app.` Copy the App-Level Token again from the same app as the two codes of
step 2 and paste it; or, if the two codes of step 2 are the wrong app's, press Left and paste the right two there. If
you closed the panel while the codes were checked, the status line says the same.

This holds for every way of pasting them, the first time you connect a workspace too. The computer that runs your
sessions checks it when it can tell both apps; if Slack does not say which app one of the codes belongs to while
nothing is connected yet, the codes are saved, and codes of two apps then stop the chat a moment later
([When Slack stops accepting the codes](#When-Slack-stops-accepting-the-codes)).

### When the panel asks

The panel asks whenever the workspace's chat is running or starting on the computer that runs your sessions and the
codes are not known to belong to its app. It does not ask, and new codes are saved at once, when Slack says they belong
to the app connected now; when the workspace's chat is not running (a first connection, or after Slack stopped
accepting its codes); or when Ship of Tools on that computer is older than this question. A Ship of Tools window older
than this question cannot answer it: it says `Slack may be connected` because the computer did not answer, which is not
so (the computer answered, and nothing was connected), and the workspace keeps its app; paste the old app's codes, or
update the window.

### If you connected a new app

A new app is in no channel, public or private, until it is invited, so each project whose channel it is not in shows
nothing. A few seconds after the codes are accepted, the connected screen names each project's channel the new app is
not in, and what to type in it. Read it before pressing Enter: Enter opens the channel list, and choosing another channel
there gives up the one the project had. To recover:

- In each channel it names, type `/invite @` and the new app's name in Slack's message box, and send it. The panel
  tries the channel again each minute, so it shows within two minutes of the invite.
- For a public channel, picking the same channel again in the list (`Ctrl+Shift+k`) also works: the app joins it.

Until then, the project's top line says `your Slack app cannot read this channel`, and the panel says what to type.

## When Slack stops accepting the codes

If Slack stops accepting the saved codes (the app was removed from the workspace, say), the first screen shows in place
of the channel and says so. If the App-Level Token held belongs to another Slack app (a colleague's, or one of your
older apps), the chat stops rather than take that app's messages, and the first screen says the code belongs to another
app. The check made when you connect refuses such codes when Slack says which app each belongs to
([If the three codes are not from one app](#If-the-three-codes-are-not-from-one-app)); this screen shows, a moment
after `chat · connected`, only when Slack did not say. Either way, Enter opens your list of Slack apps and shows step 2: click your Slack app and copy its
three codes again. If your Slack app is not in the list, press Left to make it again (step 1), and then invite it to
your projects' private channels.

The other workspaces go on. A project whose channel is in the stopped workspace names that workspace on its first
screen; there, while another workspace runs, `Ctrl+Shift+k` opens that workspace's channel list, to move the project to
a channel in it.

An older Ship of Tools connects one workspace only; if yours is older, its channel list says so, and whoever runs it
can update it.

## Setting it up by hand

These are the two files that connecting and picking from the panel write. To write them yourself instead:

1. At [api.slack.com/apps](https://api.slack.com/apps), choose **Create New App**, then **From a manifest**, pick your
   workspace, paste this (with your own name in the two names; Slack takes at most 35 characters), then **Create**,
   **Install to Workspace**, **Allow**. The settings under `settings:` are Slack's own, and the chat needs them as
   written.

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

   and `chat.toml`. Each `[[mapping]]` entry pairs one project folder (the folder as its session shows it; on Windows in
   single quotes, `folder = 'C:\Users\you\projects\my-project'`) with its one channel; `channels` must list every
   channel a mapping uses, in any workspace; and `workspace` is the workspace a mapping is in unless it names another:

   ```toml
   workspace = "T0123ABCD"
   channels = ["C0456EFGH"]

   [[mapping]]
   folder = "/home/you/projects/my-project"
   channel = "C0456EFGH"
   ```

   For a project whose channel is in another workspace, make that workspace's file beside it, named for its ID
   (`chat-secrets-T0987WXYZ.toml`), in the same four lines with that workspace's ID and the three Slack codes of the app
   made there, add the channel to `channels`, and give the project's mapping that workspace:

   ```toml
   [[mapping]]
   folder = "/home/you/projects/other-project"
   workspace = "T0987WXYZ"
   channel = "C0789IJKL"
   ```
4. Restart Ship of Tools' service on that computer (on Linux: `systemctl --user restart sotd`). Your sessions keep
   running; each session's Julia REPL (the Julia prompt at the bottom of the window) starts fresh, losing what it held. The
   service reads a file edited by hand when it starts; a change from the panel applies at once.

The full rules for both files are in [the configuration reference](../ref/config.md#chat.toml-and-chat-secrets.toml).

## If the panel shows no channel

The panel's top line says what is wrong; [The project chat](chat.md#What-the-top-line-says) lists every case. The ones
met while setting up:

- `no channel`: the project has no channel yet. Press Enter in the panel to pick one. If you wrote `chat.toml` by hand,
  its `folder` is not exactly the project's folder.
- `your Slack app cannot read this channel`: invite your Slack app to the channel, as above.
- `the chat on <computer> is not set up correctly`: a file was refused. Run `sotd log-path` in a terminal on that
  computer: it prints where the service's log is, and the log says which file and which line. A Slack code or ID is
  never printed in the log.
- `the chat does not run on <computer>`: the project's session runs on a computer other than the one named on the
  `hub =` line of `hosts.toml`.
