# rust/log/src/attach_client/rules: the attach client's rulings as pure state machines (capsule)

Every rule a viewer follows (take, outstanding input, reconnect, attach notice, fe_down, quit) is a function of an
event, with no I/O, so it runs in unit tests on every platform; the worker applies them to a live lane. Part of
capsule; charter: rust/log/CLAUDE.md.

## Files
- `mod.rs`: the module doc listing the six lettered rulings; re-exports each file's items
- `notice.rs`: (e) the attach notice and (f) the fe_down marker (`attach_notice_text`, `FeDownBaseline`)
- `outstanding.rs`: (c) the outstanding input and its resend decision (`OutstandingSlot`)
- `quit.rs`: (a) the quit dispatcher (`QuitDispatcher`, `QUIT_CUTOFF`)
- `reconnect.rs`: (d) reconnect backoff and the terminal decision (`ReconnectState`)
- `take.rs`: (b) take-on-first-input as a transaction (`TakeTransaction`)

## Start here
`mod.rs`'s module doc, then the ruling's file; each file's tests sit at its end.

## Rules
- No I/O and no OS call in this folder.
- The take queue holds one wire input (`TAKE_QUEUE_CAP` is `wire::MAX_INPUT_PAYLOAD_LEN`); a larger paste is cut there
  and the rest discarded visibly and counted (`TakeTransaction`).
- An outstanding input is resent with its idempotency key only within the same voyage; across a voyage change it is
  cancelled and reported (`OutstandingSlot::resend_after_reconnect`); a delivery-unknown outcome is never retried.
- Reconnect backoff doubles from 250 ms to a 4 s cap on platform's `Redial` and starts over only after an attach that
  lasted `STABLE` (60 s); an unresponsive supervisor turns terminal after `HEALTH_WINDOW` (120 s) (`ReconnectState`).
- Quit waits for `record_closed` then `record_verified`, stops waiting at `QUIT_CUTOFF` (90 s) with the outcome unknown,
  and never exits on that expiry (`QuitDispatcher`).
