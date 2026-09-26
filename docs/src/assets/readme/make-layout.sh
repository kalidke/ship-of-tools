#!/usr/bin/env bash
# Regenerate ../layout-labelled.png: the layout labelled on a real frame.
# The source (argument 1) is the layout.png that `docs-media.sh agent` keeps
# in dev/output/docs-media-agent (agent_cut runs this): its navigate layout
# with the drawer open (under the navigation and preview columns) and a run
# of the script in it, and the agent column full height, a live Claude Code
# session in it. Each region is outlined on the frame; the labels stay off
# the content: the three columns' labels sit in a band added above the
# frame, the drawer's in the empty rows right of its timing and prompt. The frame is
# cut above its bottom border (the fe/be version label). Adjust a box
# (x0 y0 x1 y1, in frame pixels) if the pane borders moved.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
src="${1:?usage: make-layout.sh <layout.png>}"
out="$here/../layout-labelled.png"
BAND=76                     # the label band's height above the frame
args=()
label() {   # <centre x> <centre y> <label> — in output pixels
    local cx=$1 cy=$2 label=$3 w=$(( ${#3} * 24 + 40 ))
    args+=(-fill '#0b0d11e6' -stroke '#6d8cf0' -strokewidth 2
           -draw "roundrectangle $((cx - w / 2)),$((cy - 30)) $((cx + w / 2)),$((cy + 30)) 10,10"
           -fill '#e6e9f0' -stroke none -font DejaVu-Sans-Bold -pointsize 38
           -gravity NorthWest -annotate "+$((cx - w / 2 + 20))+$((cy - 23))" "$label")
}
region() {  # <x0> <y0> <x1> <y1> <label> [label centre x y, frame pixels] — the box, and the label above it unless placed
    local x0=$1 y0=$(($2 + BAND)) x1=$3 y1=$(($4 + BAND))
    args+=(-fill none -stroke '#6d8cf0' -strokewidth 3
           -draw "rectangle $((x0 + 6)),$((y0 + 6)) $((x1 - 6)),$((y1 - 6))")
    if [ $# -gt 5 ]; then label "$6" $(($7 + BAND)) "$5"; else label $(( (x0 + x1) / 2 )) $((BAND / 2)) "$5"; fi
}
region   18  34  354  482 navigation
region  354  34  857  482 preview
region  857  34 1906 1012 agent
region   18 490  857 1012 drawer 560 982
convert "$src" -crop 1920x1014+0+0 +repage -background "#1b1b1f" -gravity North -splice 0x$BAND "${args[@]}" "$out"
