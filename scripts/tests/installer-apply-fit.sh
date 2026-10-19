# installer-apply-fit.sh -- sourced by installer-apply.sh inside its real-sotd branch (its fixtures, helpers and
# `real_apply`), never run: the applier checks the fit its armer did not, the prepare's depth against this host's role.

# The role an install gets: with no topology, the record; with this one, `daemon` decides alone, and a `frontend` entry or
# a host naming neither key (a guest of the install's home) takes the record.
recorded_apply() {  # <name> <self host> <daemon bit in install.json>: a light prepare over a fixture, applied as that host
    PREP=light real_apply "$1" v9.9.8 9.9.9
    sed -i "s/\"service\": \"systemd\"/\"service\": \"systemd\",\n  \"daemon\": $3/" "$RD/prefix/install.json"
    printf 'hub = "hubbox"\n[host.hubbox]\ndaemon = true\n[host.laptop]\nfrontend = true\n[host.guest]\n' > "$RD/hosts.toml"
    SOT_HOSTS="$RD/hosts.toml" SOT_SELF_HOST="$2" run_apply "$RD"
}
# This home names no topology, so the install is a backend role.
PREP=light real_apply pr8 v9.9.8 9.9.9
run_apply "$RD"
check "an env-less prepare onto a backend role: refused, the reason names the environments" "v9.9.8 gone 1" "$(installed_tag "$RD") $(armed "$RD") $(grep -c 'did not instantiate the Julia environments' "$RD/out" || true)"
PREP=none real_apply pr9 v9.9.8 9.9.9
run_apply "$RD"
check "a stage with no prepared.json: refused, the reason says so" "v9.9.8 gone 1" "$(installed_tag "$RD") $(armed "$RD") $(grep -c 'no record of a prepare' "$RD/out" || true)"
recorded_apply pr10 laptop false
check "a frontend entry whose install runs no daemon: the light prepare applies" "v9.9.9 gone" "$(installed_tag "$RD") $(armed "$RD")"
recorded_apply pr11 laptop true
check "a frontend entry whose install runs a daemon: the light prepare is refused" "v9.9.8 gone 1" "$(installed_tag "$RD") $(armed "$RD") $(grep -c 'did not instantiate the Julia environments' "$RD/out" || true)"
recorded_apply pr12 guest true
check "a guest of the home applying onto an install that runs a daemon: the light prepare is refused" "v9.9.8 gone 1" "$(installed_tag "$RD") $(armed "$RD") $(grep -c 'did not instantiate the Julia environments' "$RD/out" || true)"
