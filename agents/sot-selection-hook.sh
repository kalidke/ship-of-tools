#!/usr/bin/env bash
# sot-selection-hook.sh — the UserPromptSubmit hook that gives a row's agent the text its person selected in the window.
# The daemon publishes the selection as the hook's whole output in the file $SOT_SELECTION_FILE names (a row leg's
# environment carries it); this prints that file, or nothing when the variable is unset or empty or names no readable
# regular file, and always exits 0: a hook that fails or prints an error would interrupt the person's prompt.
# The host writes the prompt to this hook's stdin: read it all first, or a long prompt meets a closed pipe and the host
# shows an error.
cat > /dev/null 2>&1
f="${SOT_SELECTION_FILE:-}"
if [ -n "$f" ] && [ -f "$f" ] && [ -r "$f" ]; then
    cat -- "$f" 2> /dev/null
fi
exit 0
