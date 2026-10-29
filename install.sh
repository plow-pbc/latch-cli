#!/bin/sh
# Installs latch from its releases on releases.plow.co: the newest one, or
# the tag given first. Options go on to the release's install-binary.sh.
# Once installed, `latch update` installs newer builds. --client-only
# installs latch alone, for a machine that only asks hosts (an agent's image).
#
#   sh install.sh [TAG|latest] [--bin-dir DIR] [--state-dir DIR] [--browser chrome|camoufox]... [--no-browser] [--client-only] [--non-interactive] [--no-modify-path]
set -eu
version=latest
case "${1:-}" in
  -h | --help)
    echo 'usage: install.sh [TAG|latest] [--bin-dir DIR] [--state-dir DIR] [--browser chrome|camoufox]... [--no-browser] [--client-only] [--non-interactive] [--no-modify-path]'
    exit 0
    ;;
  "" | -*) ;;
  *)
    version=$1
    shift
    ;;
esac
releases=https://releases.plow.co/latch-cli
# The minisign public key (id DA695E5C4D2D36B9) releases are signed with, as
# latch update checks them: nothing installs unless the release's SHA256SUMS,
# and the latest.json that names it, are signed with this key. A signed
# latest.json names a promoted release, not necessarily the newest: an older
# one, served again, still checks out.
release_key=RWS5Ni1NXF5p2g7U9HvReN/6scLD8gpzhEZVW//t8Dkn4SyhqAdExFTu
command -v curl >/dev/null || { echo 'install curl first' >&2; exit 1; }
case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) target=aarch64-apple-darwin ;;
  Darwin/x86_64) target=x86_64-apple-darwin ;;
  Linux/x86_64) target=x86_64-unknown-linux-musl ;;
  Linux/aarch64|Linux/arm64) target=aarch64-unknown-linux-musl ;;
  *) echo 'unsupported OS/architecture' >&2; exit 1 ;;
esac
case "$target" in
  *-linux-*) command -v openssl >/dev/null || { echo "install OpenSSL 3 first: it checks the release's signature" >&2; exit 1; } ;;
esac
caller=$(pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
# The release's files, apart from where its archive is unpacked: nothing the
# archive carries can stand in for one of them.
mkdir "$tmp/release" "$tmp/unpacked"
download() {
  curl -fsSL --proto '=https' -o "$tmp/release/${1##*/}" "$releases/$1" || { echo "could not download $1" >&2; exit 1; }
}
# `latest` is the release latest.json names: the newest promoted one.
if [ "$version" = latest ]; then
  download latest.json
  download latest.json.minisig
  version=$(sed -n 's/.*"tag": *"\([^"]*\)".*/\1/p' "$tmp/release/latest.json")
fi
printf '%s\n' "$version" | LC_ALL=C grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$' || { echo 'expected a release tag such as v0.1.0-1327, or latest' >&2; exit 1; }
archive="latch-$version-$target.tar.gz"
for file in "$archive" SHA256SUMS SHA256SUMS.minisig; do
  download "releases/$version/$file"
done
cd "$tmp"
setup="$tmp/unpacked/latch-$version-$target"
# Nothing in the archive runs until SHA256SUMS is signed with the release key
# and lists the archive. A Mac has no tool for minisign's signatures, so there
# the release's own latch checks them, once codesign and Gatekeeper vouch that
# Plow signed its app: a Mac unpacks the archive first, to reach that app. On
# Linux, openssl checks minisign's Ed25519 signature of the file's BLAKE2b-512
# hash, and the archive is unpacked only once it checks out.
if [ "$(uname -s)" = Darwin ]; then
  tar -xzf "release/$archive" -C unpacked
  app="$setup/Plow Latch.app"
  codesign --verify --deep --strict \
    -R='anchor apple generic and identifier "co.plow.latch.cli" and certificate leaf[subject.OU] = "3559PD337Z"' "$app" &&
    spctl --assess --type execute "$app" ||
    { echo "the app in $archive is not one Plow signed; nothing installed" >&2; exit 1; }
fi
signed() {
  if [ "$(uname -s)" = Darwin ]; then
    "$app/Contents/MacOS/latch" verify-signature --key "$release_key" "release/$1" >/dev/null
  else
    printf '%s\n' "$release_key" | openssl base64 -d -A > release.key &&
      sed -n 2p "release/$1.minisig" | openssl base64 -d -A > signature &&
      # The key and signature bytes follow minisign's 2-byte algorithm and
      # 8-byte key id; the DER header makes the key one openssl reads.
      { printf '\060\052\060\005\006\003\053\145\160\003\041\000'; dd if=release.key bs=1 skip=10 2>/dev/null; } > release.der &&
      dd if=signature of=signature.ed25519 bs=1 skip=10 2>/dev/null &&
      openssl dgst -blake2b512 -binary "release/$1" > hash &&
      openssl pkeyutl -verify -pubin -keyform DER -inkey release.der -rawin -in hash -sigfile signature.ed25519 >/dev/null
  fi || { echo "$1 is not signed with latch's release key; nothing installed" >&2; exit 1; }
}
signed SHA256SUMS
[ ! -e release/latest.json ] || signed latest.json
# Verify only the selected archive; other targets need not be downloaded.
cd release
awk -v name="$archive" '$2 == name {print; found=1} END {if (!found) exit 1}' SHA256SUMS > ../selected.sha256
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum -c ../selected.sha256
else
  shasum -a 256 -c ../selected.sha256
fi
cd "$tmp"
[ "$(uname -s)" = Darwin ] || tar -xzf "release/$archive" -C unpacked
cd "$caller"
client_only=""
for option in "$@"; do
  [ "$option" != --client-only ] || client_only=1
done
if [ -n "$client_only" ]; then
  echo 'Binary defaults to ~/.local/bin (override with --bin-dir); installing latch alone, for asking hosts.'
else
  echo 'Binary defaults to ~/.local/bin (override with --bin-dir); gog uses ~/.latch/plugins (override with --state-dir).'
fi
if [ "$(uname -s)" = Darwin ]; then
  echo 'On macOS latch lives in /Applications/Plow Latch.app (~/Applications if you cannot write /Applications); ~/.local/bin/latch links to it.'
fi
[ -n "$client_only" ] || echo 'Linux hosting needs bubblewrap/user namespaces; setup may ask for sudo.'
sh "$setup/install-binary.sh" "$@" "$setup/latch"
"$setup/latch" --version
