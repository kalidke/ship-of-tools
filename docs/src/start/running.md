# What keeps running when the window closes

The window and the sessions on its computer end together: closing the window ends the backend on that computer and every session it runs, agents included. The next window starts with no sessions.

## Closing the window

- The window's close button, or the system's close, ends everything: the window waits up to 125 seconds for the sessions to end. A window that crashes or is killed counts as closed.
- `Ctrl+Q`, in any pane but the agent and the Terminal drawer, asks **Keep the daemon and sessions running?** in a dialog in the middle of the window. No is the default. `Tab` selects Yes, `Enter` confirms, and `Esc` cancels and leaves the window open; a click on No or Yes answers, and a click anywhere else cancels. Yes closes the window and leaves the backend and every session running; the next window shows them again. Closing the window before it has gone ends everything instead, as the close button does. In the agent pane and the Terminal drawer the program keeps `Ctrl+Q` (it is flow control, readline's quoted-insert and vim's literal insert), so the status line says to press it in the file or preview pane instead.
- While the window closes, a dialog in the middle of the window, and the bottom of the navigation pane, say what it waits for: `closing: waiting for this computer's backend to end the sessions`. If a session cannot be ended, the window shows how many are still running before it closes. Those sessions stay listed; end them in Sessions mode with `Shift+D`.
- If, after Yes, this computer's backend does not answer the window in time, the window stays open and asks again, saying the keep was not done; the sessions keep running.
- When the window relaunches itself for an update, the sessions wait one minute for the new window. If none opens, they end as on a close. A relaunch that cannot be done because the backend did not answer in time leaves the window open and says why in a dialog in the middle of the window, which any key dismisses.

## Two windows on one computer

Sessions end when the last window on the computer closes. Closing one of two leaves everything running for the other.

## Windows on other computers

A window that reaches a backend on another computer never ends sessions there: they keep running when it closes, and the next window reattaches. Closing the last window on the computer that runs the backend ends every session there, including any that a window on another computer was viewing.

Every computer that runs a window also runs its own private backend, so a computer that only runs windows still opens sessions of its own, and they end with its last window. The one exception is an install made with `--backend <ssh-alias>`, whose window reaches only that remote backend.

## When closing will not end sessions

A line at the bottom of the navigation pane says so when this window holds no claim on this computer's backend: the backend could not verify the window, the backend is older than the window, or there is no backend on this computer. Closing such a window leaves every session running.

## At boot

On a Linux standard install the backend starts at boot as a user service. Elsewhere, opening the window starts it.
