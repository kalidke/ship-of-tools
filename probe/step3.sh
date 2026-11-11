( cd ~ || exit 1
  tag=$(curl -fsSLI -o /dev/null -w '%{url_effective}' https://github.com/kalidke/ship-of-tools/releases/latest); tag=${tag##*/}
  case "$tag" in v[0-9]*) echo "newest release: $tag" ;; *) echo "STOP: cannot reach GitHub"; exit 1 ;; esac
  f=$(mktemp) || exit 1
  if curl -fsSL -o "$f" "https://raw.githubusercontent.com/kalidke/ship-of-tools/$tag/scripts/install.sh"; then
    SOT_INSTALL_TAG="$tag" bash "$f"; rc=$?
  else
    echo "STOP: could not fetch the installer of $tag"; rc=1
  fi
  rm -f "$f"; exit "$rc"
) 2>&1 | tee ~/sot-update.log
