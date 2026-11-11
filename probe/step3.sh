cd ~; tag=$(curl -fsSLI -o /dev/null -w '%{url_effective}' https://github.com/kalidke/ship-of-tools/releases/latest); tag=${tag##*/}; echo "newest release: $tag"
f=$(mktemp) && curl -fsSL -o "$f" "https://raw.githubusercontent.com/kalidke/ship-of-tools/$tag/scripts/install.sh" && env -u GITHUB_TOKEN -u GH_TOKEN SOT_INSTALL_TAG="$tag" bash "$f"
