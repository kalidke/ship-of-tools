# Ship of Tools REPL-streaming demo (ADR 0009 phase-2). Two things to watch:
#
#   1. LIVE STREAMING — run with `r` (fresh) or `R` (include), or paste into the
#      REPL drawer. Each "tick" line should appear ONE AT A TIME, ~1s apart, as
#      the eval runs — NOT all at once at the end. That proves frames stream as
#      `repl.frame` evts while the eval is still in flight.
#
#   2. INTERRUPT — while the loop is ticking, interrupt the REPL (Ctrl-C in the
#      REPL pane / your interrupt keybind). It should stop within ~1s with an
#      InterruptException, and the drawer should unblock immediately instead of
#      waiting out the full ~15s. That proves eval-in-task + real repl.interrupt.
#
# Self-contained: no prior REPL state needed.

const TICKS = 15

println("[03_stream] starting — $(TICKS) ticks, ~1s apart; interrupt me anytime")
flush(stdout)

elapsed = 0.0
for i in 1:TICKS
    println("[03_stream] tick $i / $TICKS")
    flush(stdout)          # push the line so it streams now, not at loop end
    sleep(1)               # a yield point — keeps the dispatch loop free to
                           # receive repl.interrupt, and lets the pipe reader
                           # emit this tick as its own stdout frame
    global elapsed += 1.0
end

println("[03_stream] done — ran $(elapsed)s without interruption")
flush(stdout)

# Trailing value → a `value` frame after the stdout frames.
(ticks = TICKS, elapsed_s = elapsed, status = :completed)
