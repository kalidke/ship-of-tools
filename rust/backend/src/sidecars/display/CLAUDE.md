# rust/backend/src/sidecars/display: a user-code child's own X display (sidecars)

On Linux each Julia child that runs user code (the REPL, and Pluto's server, whose notebook workers inherit its
environment) gets an X display of its own, so GLMakie (GLFW) can start and render: an Xvfb that only that child's
cookie opens, started before the child and ended after it. Part of the sidecars; charter:
rust/backend/src/sidecars/CLAUDE.md.

## Files
- `mod.rs`: `Display` (`start`, `apply`, `ended`, `end`), the server's arguments and the cookie file, with the tests of
  a start, its failures, its cookie, its end under its owner and its end.

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
  and left unreaped) for the child's whole life, then ends the child's tree and says why (`ChildTmp::display_ended`;
  the REPL's `ReplEnded::DisplayEnded`, Pluto's `Cause::Display`). Once the server is gone any account can bind the
  display's abstract name, and a child that opened its display only then would send its cookie and its pixels to that
  account's server. A display is never started again under a live child; the child's next start gets a new one.
- Limits: at its start Xvfb's xkbcomp writes `/tmp/server-<n>.xkm`, which the server removes before it accepts
  connections, so a server killed in its first moments can leave that file. `-displayfd` makes no lock file, so a
  program that picks a display number by lock files alone (`xvfb-run -a`, which starts at 99) can choose a number one
  of these servers holds, and its own server then fails to start. The child is ended a moment after its server's
  exit, not at the same instant. A descendant that left the child's process group keeps the ended display's name in
  its environment: Pluto's notebook workers (Distributed starts each in a session of its own and, measured, a worker
  whose server was killed lives on), and a process user code starts detached (ADR 0050 residual 7). A SIGKILL of the
  daemon's lifetime guard (residual 7) ends the daemon but not the server: unlike the Julia children, which end when
  their input closes, an Xvfb never ends on its own, so outside the systemd unit, whose cgroup ends it, it runs until
  it is killed.
