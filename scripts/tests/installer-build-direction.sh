# installer-build-direction.sh -- sourced by installer-entry.sh, never run, inside case entry_takes_the_installed_channel
# ($d is its fixture, prelude its runner): a build of the line (a + in its version) with no --version stops, and says which
# way the newest release moves it by ancestry, the release's COMMIT against the build's commit in the install's base clone.

# A build of the line is told which way the newest release moves it by ancestry: the release's COMMIT against the build's
# commit in the install's base clone (here v9.9.9's commit; the build is v9.9.8's, then a commit past v9.9.9, then unknown).
sha_in() { git -C "$d/origin" rev-parse --short=9 "$1"; }
stop_said() { say_prefix="FAIL: target: this computer runs $1, a build of the line, not a release: install a pack of the line with --archive <folder>; $2; ask the person who runs it which they want"; said "$say_prefix"; }
built="0.6.6-rc9.14-dev+$(sha_in v9.9.8)"
entry_record "$d" "$built" 1 0 none
prelude --be-only
check "a build with a + stops (exit 2)" "2" "$ENTRY_RC"
check "a build the newest release contains is told forward, not older" "1" "$(stop_said "$built" 'the newest release, v9.9.10-rc1, contains this build: naming it with --version moves this computer forward')"
check "the stop never says a named release is usually older" "0" "$(said 'usually older')"
check "it ends with that one line" "yes" "$(case "$ENTRY_LAST" in ('FAIL: '*) echo yes ;; (*) echo no ;; esac)"
check "it fetched no installer and no tarball" "0" "$(count_in "$d/ctl/net.log" 'raw.githubusercontent\|releases/latest\|\.tar\.gz')"
check "it read the release list and the release's COMMIT only" "2" "$(count_in "$d/ctl/net.log" 'api.github.com\|releases/download/v9.9.10-rc1/COMMIT')"
git -C "$d/origin" -c user.name=t -c user.email=t@t commit -q --allow-empty -m three
git -C "$d/home/.local/share/sot/repo/base" fetch -q origin
built="0.6.6-rc9.14-dev+$(sha_in HEAD)"
entry_record "$d" "$built" 1 0 none
prelude --be-only
check "a build past the newest release is told it moves back" "1" "$(stop_said "$built" 'the newest release, v9.9.10-rc1, does not contain this build: naming it with --version moves this computer back')"
built="0.6.6-rc9.14-dev+abc123"
entry_record "$d" "$built" 1 0 none
prelude --be-only
check "a build whose commit the clone lacks is told so, in one plain line" "1" "$(stop_said "$built" 'could not tell which way the newest release, v9.9.10-rc1, moves this computer (this build'"'"'s commit abc123 is not in '"$d"'/home/.local/share/sot/repo/base)')"
# The rule also answers for a named tag when asked directly (no entry asks it of an explicit run), and a record's 40-digit
# commit stands in for the version's +<sha>.
P="$d/home/.local/share/sot"
check "a named release the build is in is forward" "forward" "$(CTL="$d/ctl" PATH="$d/stubs:$PATH" HOME="$d/home" installer_build_direction "$P" "0.6.6-rc9.14-dev+$(sha_in v9.9.8)" v9.9.9)"
git -C "$d/origin" rev-parse HEAD > "$d/ctl/head"
installer_manifest_json "$P" "$d/home/.config/sot" none "$built" "v$built" "$(git -C "$d/origin" rev-parse v9.9.8)" 2026-10-01T00:00:00Z "" 1 0 > "$P/install.json"
check "a record's full commit decides when the version holds none" "forward" "$(CTL="$d/ctl" PATH="$d/stubs:$PATH" HOME="$d/home" installer_build_direction "$P" "0.6.6-rc9.14-dev+src" v9.9.9)"
check "a build that names no commit is not guessed" "unknown: this build names no commit" "$(rm -f "$P/install.json"; installer_build_direction "$P" "0.6.6-rc9.14-dev+src" v9.9.9)"

# A pack of the line installed with --archive on a build of the line reads the folder and nothing else: no curl call, and no
# ancestry line (the archive's own commit is in hand, and its tag is not on GitHub).
d2="$WORK/offplus"; entry_fixture "$d2"
printf '#!/bin/sh\necho "curl $*" >> "$CTL/net.log"\nexit 7\n' > "$d2/stubs/curl"; chmod +x "$d2/stubs/curl"
entry_record "$d2" "0.6.6-rc9.14-dev+$(git -C "$d2/origin" rev-parse --short=9 v9.9.8)" 1 0 none
entry_archive "$d2" --be-only --no-service
check "an archive install on a build of the line passes" "0 PASS" "$ENTRY_RC $(printf '%s' "$ENTRY_LAST" | cut -c1-4)"
check "it made no curl call" "no" "$([ -s "$d2/ctl/net.log" ] && echo yes || echo no)"
check "it printed no ancestry line" "0" "$(said 'by ancestry')"

# Every git call of installer_build_direction carries the stall bound (a blobless clone's `cat-file -e` runs a lazy fetch).
fn_text="$(sed -n '/^installer_build_direction() {/,/^}/p' "$ENTRY_SRC/scripts/install.sh")"
check "every git call in installer_build_direction carries GIT_BOUND" "$(printf '%s\n' "$fn_text" | grep -c 'git ')" "$(printf '%s\n' "$fn_text" | grep -c 'git "${GIT_BOUND\[@\]}"')"
# So does every git call of the base clone's step (an --archive install's rev-parse runs a lazy fetch in a blobless clone).
base_text="$(sed -n '/^if \[ ! -d "\$BASE" \]; then/,/^# ---- 3b/p' "$ENTRY_SRC/scripts/install.sh" | grep -v '^ *#')"
check "the base clone's step has git calls" "yes" "$([ "$(printf '%s\n' "$base_text" | grep -cE '(^|[ !(])git ')" -ge 4 ] && echo yes || echo no)"
check "every git call in the base clone's step carries GIT_BOUND" "$(printf '%s\n' "$base_text" | grep -cE '(^|[ !(])git ')" "$(printf '%s\n' "$base_text" | grep -cE '(^|[ !(])git "\$\{GIT_BOUND\[@\]\}"')"
