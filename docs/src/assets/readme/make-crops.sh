#!/usr/bin/env bash
# Regenerate the README stills: each is a crop of one region of a 1920x1080
# docs still in ../media/ (itself a crop of the window, see STILL_CROP in
# scripts/docs-media.sh). Re-run after the media stills are re-shot; adjust a
# geometry (WxH+X+Y, in still pixels) if the region moved.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
media="$here/../media"
crop() {  # <source still> <geometry> <output>
    convert "$media/$1" -crop "$2" +repage "$here/$3"
}
crop state-colors.png 800x226+4+104 sessions-crop.png
crop repl-figure.png  690x512+6+490 repl-crop.png
