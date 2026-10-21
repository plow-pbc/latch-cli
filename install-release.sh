#!/bin/sh
# Download a private GitHub release and run its pinned plugin installer.
set -eu
[ "$#" -ge 1 ] || { echo 'usage: install-release.sh vVERSION [--state-dir DIR]' >&2; exit 1; }
version=$1
shift
printf '%s\n' "$version" | LC_ALL=C grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$' || { echo 'expected a v-prefixed release version' >&2; exit 1; }
case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) target=aarch64-apple-darwin ;;
  Darwin/x86_64) target=x86_64-apple-darwin ;;
  Linux/x86_64) target=x86_64-unknown-linux-musl ;;
  Linux/aarch64|Linux/arm64) target=aarch64-unknown-linux-musl ;;
  *) echo 'unsupported OS/architecture' >&2; exit 1 ;;
esac
command -v gh >/dev/null || { echo 'install GitHub CLI and run gh auth login first' >&2; exit 1; }
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
echo 'Installing into ~/.local/bin; gog uses ~/.latch/plugins unless --state-dir is supplied.'
echo 'Linux hosting needs bubblewrap/user namespaces; setup may ask for sudo.'
sh "$setup/install.sh" "$@" "$setup/latch"
"$setup/latch" --version
echo 'An already-running latch serve keeps its old binary; restart it intentionally to update.'
