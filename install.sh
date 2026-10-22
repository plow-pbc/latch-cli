#!/bin/sh
# Installs latch from a private GitHub release: the newest published one, or
# the tag given first. Options go on to the release's install-binary.sh.
#
#   sh install.sh [TAG|latest] [--bin-dir DIR] [--state-dir DIR] [--browser chrome|camoufox]... [--no-browser]
set -eu
version=latest
case "${1:-}" in
  -h | --help)
    echo 'usage: install.sh [TAG|latest] [--bin-dir DIR] [--state-dir DIR] [--browser chrome|camoufox]... [--no-browser]'
    exit 0
    ;;
  "" | -*) ;;
  *)
    version=$1
    shift
    ;;
esac
command -v gh >/dev/null || { echo 'install GitHub CLI and run gh auth login first' >&2; exit 1; }
# `latest` is the newest published release; drafts are never picked.
if [ "$version" = latest ]; then
  version=$(gh release view --repo plow-pbc/latch-cli --json tagName --jq .tagName)
fi
printf '%s\n' "$version" | LC_ALL=C grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$' || { echo 'expected a release tag such as v0.1.0-1327, or latest' >&2; exit 1; }
case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) target=aarch64-apple-darwin ;;
  Darwin/x86_64) target=x86_64-apple-darwin ;;
  Linux/x86_64) target=x86_64-unknown-linux-musl ;;
  Linux/aarch64|Linux/arm64) target=aarch64-unknown-linux-musl ;;
  *) echo 'unsupported OS/architecture' >&2; exit 1 ;;
esac
caller=$(pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
archive="latch-$version-$target.tar.gz"
gh release download "$version" --repo plow-pbc/latch-cli --pattern "$archive" --pattern SHA256SUMS --dir "$tmp"
cd "$tmp"
# Verify only the selected archive; other targets need not be downloaded.
awk -v name="$archive" '$2 == name {print; found=1} END {if (!found) exit 1}' SHA256SUMS > selected.sha256
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum -c selected.sha256
else
  shasum -a 256 -c selected.sha256
fi
tar -xzf "$archive"
setup="$tmp/latch-$version-$target"
cd "$caller"
echo 'Binary defaults to ~/.local/bin (override with --bin-dir); gog uses ~/.latch/plugins (override with --state-dir).'
if [ "$(uname -s)" = Darwin ]; then
  echo 'On macOS latch lives in ~/Applications/Plow Latch.app; ~/.local/bin/latch links to it.'
fi
echo 'Linux hosting needs bubblewrap/user namespaces; setup may ask for sudo.'
sh "$setup/install-binary.sh" "$@" "$setup/latch"
"$setup/latch" --version
echo 'An already-running latch serve keeps its old binary; restart it intentionally to update.'
