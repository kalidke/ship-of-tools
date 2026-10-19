# installer-entry-julia.sh -- sourced by installer-entry.sh (its fixture, runner and helpers), never run: a Julia
# environment that was never instantiated fails naming it, and a Julia below the kernel's compat floor stops the
# install before any step runs on it, naming the version.

# ---------------------------------------------------------------------------
case_start "entry_names_a_julia_it_cannot_use"
d2="$WORK/vd-julia"; entry_fixture "$d2"
cat > "$d2/stubs/julia" <<'JU'
#!/bin/sh
case "$*" in *update_comm*) mkdir -p "$SOT_COMM_HOME/bin"; git -C "${2#--project=}" rev-parse --short=9 HEAD > "$SOT_COMM_HOME/VERSION" ;; *print\(VERSION\)*) printf '1.12.0' ;; esac
exit 0
JU
entry_archive "$d2" --be-only --no-service
check "a Julia environment that was never instantiated: exit 1" "1" "$ENTRY_RC"
starts_with "its FAIL names the environment" "FAIL: julia: julia/kernel has no Manifest.toml" "$ENTRY_LAST"
d2="$WORK/vd-julia-floor"; entry_fixture "$d2"
printf '1.11.7' > "$d2/ctl/julia-ver"; : > "$d2/ctl/no-channel"
entry_archive "$d2" --be-only --no-service
starts_with "a Julia below the kernel's floor stops the install before any step, naming the version" "FAIL: julia: the daemon runs Julia 1.11.7, below the 1.12 the kernel needs: run " "$ENTRY_LAST"
check "and no step ran on it" "0" "$(count_in "$d2/ctl/julia.log" 'update_comm\|Pkg.instantiate')"
