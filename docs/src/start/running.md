# What keeps running when the window closes

The window and the sessions on its computer end together: closing the window ends the backend on that computer and every session it runs, agents included. The next window starts with no sessions.

## Closing the window

- The window's close button, or the system's close, ends everything: the window shows `closing…` and waits up to 125 seconds for the sessions to end. A window that crashes or is killed counts as closed.
- `Ctrl+Q`, with the navigation pane focused, asks **Keep the daemon and sessions running?** No is the default. `Tab` selects Yes, `Enter` confirms, and `Esc` cancels and leaves the window open. Yes closes the window and leaves the backend and every session running; the next window shows them again. Closing the window before it has gone ends everything instead, as the close button does.
- While sessions end, the bottom of the navigation pane reads `closing…`. If a session cannot be ended, the window shows how many are still running before it closes, and the next window shows the count again. If this computer's backend does not confirm the close or the keep, the window says so before it closes. Those sessions stay listed; end them in Sessions mode with `Shift+D`.
- When the window relaunches itself for an update, the sessions wait one minute for the new window. If none opens, they end as on a close.

## Two windows on one computer

Sessions end when the last window on the computer closes. Closing one of two leaves everything running for the other.

## Windows on other computers

A window that reaches a backend on another computer never ends sessions there: they keep running when it closes, and the next window reattaches. Closing the last window on the computer that runs the backend ends every session there, including any that a window on another computer was viewing.

## When closing will not end sessions

A line at the bottom of the navigation pane says so when this window holds no claim on this computer's backend: the backend could not verify the window, the backend is older than the window, or there is no backend on this computer. Closing such a window leaves every session running.

## At boot

On a Linux standard install the backend starts at boot as a user service, with no sessions. Elsewhere, opening the window starts it.
