# rust/backend/src/sidecars/display: a user-code child's own X display (sidecars)

On Linux each Julia child that runs user code (the REPL, and Pluto's server, whose notebook workers inherit its
environment) gets an X display of its own, so GLMakie (GLFW) can start and render: an Xvfb that only that child's
cookie opens, started before the child and ended after it. Part of the sidecars; charter:
rust/backend/src/sidecars/CLAUDE.md.

## Files
- `holders.rs`: the processes that hold a display's cookie, and their end (`end`).
- `mod.rs`: `Display` (`start`, `apply`, `ended`, `end`) and its `Cookie`, the server's arguments and the cookie file,
  with the tests of a start, its failures, its cookie, its end under its owner, its end, and the end of every process
  that holds its cookie.

## Start here
`Display::start`.

## Rules
- A display belongs to its child's `ChildTmp` (`ChildTmp::for_user_code`), which ends it after the child's reap. The
  kernel runs no user code and gets none; macOS and Windows get none (GLMakie draws through Cocoa and WGL there).
- The server is `Xvfb -displayfd 1 -auth <cookie> -noreset -nolisten tcp -nolisten unix -screen 0 3840x2160x24`,
  started through `Signal::spawn` (a process group of its own): the lowest free display number, never one another
  server holds, and no lock file (`-displayfd`); the abstract socket alone, so a killed server leaves nothing in
  `/tmp/.X11-unix` (`-nolisten unix`); the cookie read once, at the start (`-noreset`: a server that resets when its
  last client leaves reads the file again, and a removed file would lock the child out).
- The cookie is one Xauthority entry of family FamilyWild with an empty address and display number, so the server and
  every client read one file written before the number is known: `MIT-MAGIC-COOKIE-1`, 16 bytes from the OS's secure
  generator, in a new 0600 file in the child's own 0700 temporary folder, never on a command line. The abstract socket
  has no permissions, so the cookie is what keeps every other account off the display.
- The child's environment gets `DISPLAY`, `XAUTHORITY` and `XDG_SESSION_TYPE=x11`, replacing any display the daemon
  inherited: GLFW 3.4 tries a Wayland session's default socket before X11 unless the session type says x11.
- A start that fails (no Xvfb, an early exit, no number within `START_BOUND`, 10 s) never fails the child: the daemon
  logs the reason at WARN, the child gets `SOT_NO_DISPLAY=<reason>` and keeps what it inherited, and the REPL shim says
  the reason at its first eval (julia/repl).
- A display that ends under a live child ends the child: the child's owner waits on `ended` (the server's exit, seen
  and left unreaped) for the child's whole life, then retires the child and says why (`ChildTmp::display_ended`; the
  REPL's `ReplEnded::DisplayEnded`, Pluto's `Cause::Display`). Once the server is gone any account can bind the
  display's abstract name, and a child that opened its display only then would send its cookie and its pixels to that
  account's server. A display is never started again under a live child; the child's next start gets a new one.
- Nothing that holds a display's cookie outlives the display. Every end of a display ends every process of the
  daemon's account whose environment names its cookie as `XAUTHORITY`, in the child's process group or out of it
  (Pluto's notebook workers, a process user code starts detached): its owner's `end` and its drop, through the
  cookie's drop, before the server's group, and its server's exit, in `ended`, at once (`holders::end`). Every end of a
  child ends its display (`ChildTmp::retire`), so a restart, a `--fresh`, a death and a row's end end those processes
  too. Each is found by a walk of `/proc`, opened as a pidfd, its environment read again while that pidfd shows it
  alive, and sent SIGKILL through the pidfd, so a pid the OS has reused is never signalled; the walks repeat until one
  finds no new holder, because a holder that forked between a walk and its kill leaves a child only the next walk sees.
  Tests pin the repeat walk (`end_with`) and the re-read (`end_one`); the pidfd is pinned only by `kill`'s signature,
  because no test can make the OS reuse a pid without root.
  `end` runs the walk on `spawn_blocking`, since it reads every process of the account; `ended` walks on its own task,
  because the walk ends the child and its owner must see `ended` return before the child's exit to name the display as
  the cause (`a_pluto_whose_display_ends_*`); a `Cookie`'s drop in sync code walks on the thread it runs on.
- Limits: at its start Xvfb's xkbcomp writes `/tmp/server-<n>.xkm`, which the server removes before it accepts
  connections, so a server killed in its first moments can leave that file. `-displayfd` makes no lock file, so a
  program that picks a display number by lock files alone (`xvfb-run -a`, which starts at 99) can choose a number one
  of these servers holds, and its own server then fails to start. The child and what holds its cookie are ended a moment
  after its server's exit, not at the same instant. A process is found by the environment it started with, so one
  started with another `XAUTHORITY`, or none, and this `DISPLAY` is not found and keeps the ended display's name. At the
  daemon's stop the Signal's fire ends the server at once, and what holds its cookie ends at its owner's close-out or,
  at the latest, by the lifetime guard within `DRAIN_BOUND` of the daemon's end. A child with no display has no cookie
  to find its detached processes by (ADR 0050 residual 7). A SIGKILL of the daemon's lifetime guard (residual 7) ends
  the daemon but not the server: unlike the Julia children, which end when their input closes, an Xvfb never ends on its
  own, so outside the systemd unit, whose cgroup ends it, it runs until it is killed.
