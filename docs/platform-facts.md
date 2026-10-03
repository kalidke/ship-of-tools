# Platform facts

Facts about the platforms Ship of Tools runs on that the 0.6.6 line had to
learn by getting them wrong. Each section gives the fact, how it shows up when
code ignores it, the code that now relies on it, and the commit that fixed it.
Paths and line numbers are as of the 0.6.6 release branch.

## A lock on the shared home excludes another machine only through one lock manager

**Fact.** A `flock` on an NFS-mounted home excludes a writer on another machine
only when both go through the same lock manager. An NFSv4 mount with
`local_lock=none` takes the lock on the server; an NFSv4 mount with any other
`local_lock` keeps it on the client, and an NFSv3 lock and an NFSv4 lock on one
export exclude nothing. So the code trusts a lock across machines only on NFSv4
with `local_lock=none`; on NFSv3 or an unknown mount it binds the comm folder
to the one machine that wrote the folder's lock record.

**When it is wrong.** Both machines are granted the lock and append to the
same inbox at once. Nothing reports an error, and an append is not atomic
across machines, so a line can be torn.

**Code.** `comm/core/scripts/comm-lib.sh:1059` (`sot_inbox_lock_identity`) and
`rust/backend/src/comm_inbox.rs:572` (`lock_identity`) compute the same
identity; a script whose identity differs from the folder's record hands the
frame to the daemon as `comm.file` instead of appending. The record,
`inbox-lock-manager`, is described at `comm/PROTOCOL.md:290`.

**Fixed in.** `45429888 comm: an NFS v4 lock counts as shared only with
local_lock=none` and `c33cf80a comm: an unknown lock binds the comm folder to
the one machine that wrote it`.

## An MSYS exec leaves a process whose Windows parent is dead

**Fact.** In git-bash, an MSYS program that execs another MSYS program becomes
a new Windows process, and its Windows parent has already exited. A walk up
Windows' parent links stops there; Cygwin's `/proc` still holds the MSYS parent
chain.

**When it is wrong.** A comm script that walked its ancestors to find the row's
agent saw only its own `bash`: the row's own agent was refused by the row rule,
and a second agent above such a script went uncounted.

**Code.** `comm/core/scripts/comm-lib.sh:1919` (`_sot_ancestor_chain`) reads
Cygwin's `/proc` while the parent is an MSYS process, then runs `sotd.exe
ancestors --from <winpid>` above the first MSYS process whose parent is native;
where that walk stops at an MSYS process it goes on from that process's Cygwin
parent. The `--from` start is `rust/backend/src/ancestors.rs:73`
(`parse_from`).

**Fixed in.** `cbc8ac1b comm: the Windows walk reads Cygwin's /proc to a native
parent, and sotd walks on from there`.

## Claude Code's prompt glyph is `❯` or `>` on Windows

**Fact.** Claude Code draws its input prompt as `❯` followed by U+00A0, on the
line between the input box's two rules; menus and dialog inputs follow the
glyph with an ASCII space instead. When its unicode check fails it draws a
plain `>`. That depends on the environment, not the OS, so a Windows row can
show either glyph. Whether the U+00A0 survives ConPTY has not been shown, so on
Windows a glyph with nothing after it also counts.

**When it is wrong.** A screen reader that knows only one glyph never finds a
free prompt on a Windows row that draws the other, and the daemon's wake, which
types only at a free prompt, never reaches that row.

**Code.** `rust/backend/src/comm_wake.rs:280` (`prompt_glyphs`) takes both
glyphs for Claude Code on Windows and `❯` elsewhere;
`rust/backend/src/comm_wake.rs:314` (`NBSP_ON_WINDOWS`) and `:316`
(`nbsp_required`) decide whether the U+00A0 is required.

**Fixed in.** `684cb17b comm wake: Windows takes both prompt glyphs inside the
box`.

## A native Windows jq writes CRLF, and MSYS rewrites a leading `/`

**Fact: carriage returns.** A native Windows `jq.exe` opens stdout in text mode
and writes every newline as `\r\n`. Bash keeps the `\r`: command substitution
strips only the final newline, and `read` and `mapfile` split on newlines only.

**When it is wrong.** A handle, host, workspace id or cursor offset read back
with a trailing `\r` never equals the clean string. In one field report every
send from a box reported `no such handle` for a delivery that had already
landed; a cursor offset failed its numeric test and read as 0.

**Code.** `comm/core/scripts/comm-lib.sh:85` (`sot_jq`) strips the `\r` from
jq's output and keeps jq's exit status. It is for identifiers only; free text
such as a message body still goes through `jq` directly, so a carriage return
the sender typed is not removed.

**Fixed in.** `10fc681e comm: a jq that writes CRLF must not fail a send or
reset a cursor` and `570f5aba fix(comm): a carriage return from a Windows jq
must not reach curl or gh`.

**Fact: argument conversion.** When git-bash runs a native Windows program, it
rewrites any argument that begins with a single `/` as a Windows path:
`/sot-session-start X` reaches the program as `C:/Program Files/Git/sot-session-start X`.
`./`, `~/`, `//server` and a slash inside the string are left alone. Setting
`MSYS2_ARG_CONV_EXCL="*"` is not a fix: it also stops jq's file arguments from
being converted, so the registry cannot be opened and every send is refused.

**When it is wrong.** A message that begins with `/` arrives changed in the
peer's inbox. In a message of several lines only the first is changed, so it
reads as a typo.

**Code.** The message body goes to jq with `--rawfile`, never as an argument:
`comm/core/scripts/comm-relay.sh:184` and `:192`, and
`comm/core/scripts/comm-send.sh:117`.

**Fixed in.** `9d98edf3 comm: capsule sessions carry a pinned identity;
slash-leading values reach jq via --rawfile on Windows (#178)`, which v0.6.5
already contains.

## Antivirus holds a freshly extracted file on Windows

**Fact.** On Windows, antivirus scanning a freshly extracted `.exe` can hold it
open for a few seconds. Renaming the directory that contains it fails with
"Access is denied" until the scanner lets go.

**When it is wrong.** The updater's download and extract succeed and the final
rename that commits the staged update fails, so a box that looks healthy never
updates; abandoned stage directories accumulate across releases.

**Code.** `rust/updater/src/lib.rs:61` (`RENAME_BACKOFF`, about a minute of
retries) and `rust/updater/src/lib.rs:483` (`commit_stage`, which retries the
rename and then sweeps every other abandoned `tmp-*` directory). When the
retries run out, the status reads `update blocked: <error>`
(`rust/backend/src/update.rs:177`).

**Fixed in.** `dda4572e updater: retry the stage-commit rename, sweep
leftovers, surface a block`, merged in `86af3239`.

## Claude Code's background service moves a conversation out of the row

*Not yet pinned to code.*

**Fact.** Claude Code has a background service ("agent view"). Typing
`/background` in a running Claude Code moves the conversation into `claude
daemon run`, started under the user's service manager (`systemd --user` on
Linux), which runs it through `claude bg-pty-host` as a fork of the
conversation under a new session id (`--fork-session --resume`). That process
is not in the row's capsule. The row's supervisor then
starts a fresh Claude Code in the row, under yet another session id. This was
measured on Claude Code 2.1.288 for `/background`; `--bg` and the hand-off when
Claude Code exits enter the same service and were not measured.

Not in that measurement: a running Claude Code re-reads its folder's
`.claude/settings.local.json`, and `disableAgentView: true` there removes the
command (`/background` then answers `Unknown command: /background`);
`CLAUDE_CODE_DISABLE_AGENT_VIEW=1` does the same for a new process.

**When it is wrong.** The row shows a new conversation while the original
carries on out of sight: the row's screen, the daemon's wake and the row's
supervisor no longer reach it.

**Code.** None at this branch head sets `disableAgentView` or the variable.

**Fixed in.** No commit yet.
