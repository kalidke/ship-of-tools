  d=$(mktemp -d) && printf 'hub = "ALIAS"\n[host.ALIAS]\ndaemon = true\n' > "$d/hosts.toml"
  SOT_HOSTS="$d/hosts.toml" ~/.local/share/sot/bin/sotd status
  