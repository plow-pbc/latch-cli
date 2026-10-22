#!/bin/sh
# Installs the pinned browser engines a host runs for `latch browser`.
#
#   sh browser-engines.sh --state-dir DIR ENGINE...
#
# ENGINE is chrome or camoufox. install-binary.sh runs this, and so does
# `latch browser setup`, which carries a copy of this script and the pins.
# The pins are read next to this script: src/browser/cft.json,
# src/browser/camoufox.json and camoufox/ (the helper's lockfile).
#
# chrome: Chrome for Testing, on macOS and on Linux arm64 (the Pi), the
# release pinned in src/browser/cft.json, into
# <state-dir>/plugins/cft/<version>/, beside older versions, with the pin
# copied in as manifest.json. That download needs curl, and ditto (macOS)
# or unzip (Linux). On Linux, Chrome also needs Xvfb, the display of a host
# with no screen: unzip and Xvfb are installed first if missing, which
# needs root, so it asks for sudo once. On another platform it says so and
# installs nothing.
#
# camoufox (macOS): the Camoufox build, the Node it runs its helper on, and
# the helper's runtime (Playwright and camoufox-js, from camoufox/'s
# lockfile, with `npm ci`), into <state-dir>/plugins/camoufox/<version>/.
# About 1 GB on disk.
#
# Running it again changes nothing that is already in place.
set -eu

say() { printf '%s: %s\n' "${0##*/}" "$*"; }
die() { say "$*" >&2; exit 1; }

usage="usage: sh browser-engines.sh --state-dir DIR chrome|camoufox..."
[ $# -ge 2 ] && [ "$1" = --state-dir ] && [ -n "$2" ] || die "$usage"
state_dir=$2
shift 2
engines=""
for engine in "$@"; do
  case "$engine" in
    chrome | camoufox) engines="$engines $engine" ;;
    *) die "$usage" ;;
  esac
done
here=$(dirname "$0")

if command -v sha256sum >/dev/null 2>&1; then
  sha256() { sha256sum < "$1" | cut -d ' ' -f 1; }
elif command -v shasum >/dev/null 2>&1; then
  sha256() { shasum -a 256 < "$1" | cut -d ' ' -f 1; }
else
  die "need sha256sum or shasum to check the browser engines"
fi
cft_tmp=""
fox_tmp=""

# Installs `$@` (package names as apt calls them) as root, under one sudo.
as_root_install() {
  if command -v apt-get >/dev/null 2>&1; then
    manager=apt-get
  elif command -v dnf >/dev/null 2>&1; then
    manager=dnf
  elif command -v apk >/dev/null 2>&1; then
    manager=apk
  elif command -v pacman >/dev/null 2>&1; then
    manager=pacman
  else
    die "no apt-get, dnf, apk or pacman found; install $* yourself, then run this again"
  fi
  # Xvfb's package has a different name on each.
  named=""
  for package in "$@"; do
    case "$manager:$package" in
      dnf:xvfb) package=xorg-x11-server-Xvfb ;;
      pacman:xvfb) package=xorg-server-xvfb ;;
    esac
    named="${named:+$named }$package"
  done
  case $manager in
    apt-get) install="apt-get update -q && DEBIAN_FRONTEND=noninteractive apt-get install -y -q $named" ;;
    dnf) install="dnf install -y $named" ;;
    apk) install="apk add $named" ;;
    pacman) install="pacman -Sy --needed --noconfirm $named" ;;
  esac
  say "installing as root: $named"
  if [ "$(id -u)" -eq 0 ]; then
    as_root=""
  elif command -v sudo >/dev/null 2>&1; then
    as_root=sudo
  else
    die "need root to install $named, and sudo is not installed; run this as root"
  fi
  $as_root sh -c "$install" || die "installing $named as root failed (see above); fix that and run this again"
}
trap 'rm -rf ${cft_tmp:+"$cft_tmp"} ${fox_tmp:+"$fox_tmp"}' EXIT

# The pinned Chrome for Testing. The pin's JSON has a fixed shape: each
# build is one block of "key": "value" lines, which sed reads. This checks
# the zip's sha256; latch hashes the whole unpacked build before every
# launch. The manifest goes in last, so a folder without it is a half-made
# install, made again.
chrome() {
  case "$(uname -s)/$(uname -m)" in
    Darwin/arm64) cft_build=mac_arm64 ;;
    Darwin/x86_64) cft_build=mac_x64 ;;
    Darwin/*) die "no Chrome for Testing build for $(uname -m)" ;;
    Linux/aarch64 | Linux/arm64) cft_build=linux_arm64 ;;
    *)
      say "no Chrome for Testing build for $(uname -s) $(uname -m): this host runs no browser"
      return 0
      ;;
  esac
  # Chrome needs these even when its build is already in place.
  if [ "$(uname -s)" = Linux ]; then
    missing=""
    command -v unzip >/dev/null 2>&1 || missing="unzip"
    command -v Xvfb >/dev/null 2>&1 || missing="${missing:+$missing }xvfb"
    [ -z "$missing" ] || as_root_install $missing
  fi
  cft_pin="$here/src/browser/cft.json"
  [ -f "$cft_pin" ] || die "$cft_pin not found; run install.sh from the latch workspace"
  cft() {
    value=$(sed -n "/^    \"$cft_build\": {/,/}/s/^ *\"$1\": *\"\([^\"]*\)\".*/\1/p" "$cft_pin")
    [ -n "$value" ] || die "$cft_pin has no builds.$cft_build.$1"
    printf '%s\n' "$value"
  }
  cft_version=$(sed -n 's/^  "version": *"\([^"]*\)".*/\1/p' "$cft_pin")
  [ -n "$cft_version" ] || die "$cft_pin has no version"
  cft_dir="$state_dir/plugins/cft/$cft_version"
  if [ -f "$cft_dir/manifest.json" ] && [ "$(cksum < "$cft_pin")" = "$(cksum < "$cft_dir/manifest.json")" ]; then
    say "Chrome for Testing $cft_version already installed in $cft_dir"
    return 0
  fi
  command -v curl >/dev/null 2>&1 || die "need curl to download Chrome for Testing"
  cft_url=$(cft url)
  cft_zip_sha256=$(cft zip_sha256)
  cft_folder=$(cft folder)
  (umask 077 && mkdir -p "$state_dir")
  mkdir -p "$cft_dir"
  cft_tmp=$(mktemp -d "$cft_dir/.download.XXXXXX")
  say "downloading Chrome for Testing $cft_version (about 200 MB)"
  curl -fsSL --proto '=https' -o "$cft_tmp/cft.zip" "$cft_url" || die "could not download $cft_url"
  got=$(sha256 "$cft_tmp/cft.zip")
  [ "$got" = "$cft_zip_sha256" ] || die "$cft_url has sha256 $got, not the pinned $cft_zip_sha256; nothing installed"
  # Each unpacks as scripts/cft-pin.py did for the pin's tree hash.
  if [ "$(uname -s)" = Darwin ]; then
    ditto -x -k "$cft_tmp/cft.zip" "$cft_tmp/x" || die "could not unpack $cft_url"
  else
    unzip -q "$cft_tmp/cft.zip" -d "$cft_tmp/x" || die "could not unpack $cft_url"
  fi
  [ -d "$cft_tmp/x/$cft_folder" ] || die "$cft_url has no $cft_folder in it"
  rm -rf "${cft_dir:?}/$cft_folder"
  mv "$cft_tmp/x/$cft_folder" "$cft_dir/$cft_folder"
  cp "$cft_pin" "$cft_dir/manifest.json"
  say "installed Chrome for Testing $cft_version to $cft_dir"
}

# The pinned Camoufox, Node and the helper's runtime, each checked as it
# arrives (the archives' sha256, `npm ci`'s lockfile integrity); latch
# hashes each unpacked part before every launch. No package's install
# script runs. The manifest goes in last.
camoufox() {
  [ "$(uname -s)" = Darwin ] || die "Camoufox is pinned for macOS hosts only"
  fox_pin="$here/src/browser/camoufox.json"
  fox_package="$here/camoufox"
  [ -f "$fox_pin" ] && [ -f "$fox_package/package-lock.json" ] || die "$fox_pin or $fox_package not found; run install.sh from the latch workspace"
  fox() { plutil -extract "$1" raw -o - "$fox_pin" 2>/dev/null || die "$fox_pin has no $1"; }
  case "$(uname -m)" in
    arm64) fox_build=builds.mac_arm64 ;;
    x86_64) fox_build=builds.mac_x64 ;;
    *) die "no Camoufox build for $(uname -m)" ;;
  esac
  fox_version=$(fox version)
  fox_dir="$state_dir/plugins/camoufox/$fox_version"
  if [ -f "$fox_dir/manifest.json" ] && [ "$(cksum < "$fox_pin")" = "$(cksum < "$fox_dir/manifest.json")" ]; then
    say "Camoufox $fox_version already installed in $fox_dir"
    return 0
  fi
  command -v curl >/dev/null 2>&1 || die "need curl to download Camoufox"
  (umask 077 && mkdir -p "$state_dir")
  mkdir -p "$fox_dir"
  # Absolute: npm runs from inside it.
  fox_tmp=$(cd "$(mktemp -d "$fox_dir/.download.XXXXXX")" && pwd)
  fetch() { # URL SHA256 FILE
    curl -fsSL --proto '=https' -o "$3" "$1" || die "could not download $1"
    got=$(sha256 "$3")
    [ "$got" = "$2" ] || die "$1 has sha256 $got, not the pinned $2; nothing installed"
  }
  say "downloading Camoufox $fox_version (about 300 MB) and Node"
  fetch "$(fox "$fox_build.camoufox.url")" "$(fox "$fox_build.camoufox.sha256")" "$fox_tmp/camoufox.zip"
  fetch "$(fox "$fox_build.node.url")" "$(fox "$fox_build.node.sha256")" "$fox_tmp/node.tar.gz"
  mkdir "$fox_tmp/camoufox"
  ditto -x -k "$fox_tmp/camoufox.zip" "$fox_tmp/camoufox" || die "could not unpack Camoufox"
  # What camoufox-js reads to know the build; scripts/camoufox-pin.py
  # writes the same bytes.
  printf '{"version":"%s","build":"%s"}' "${fox_version%%-*}" "${fox_version#*-}" > "$fox_tmp/camoufox/version.json"
  tar -xzf "$fox_tmp/node.tar.gz" -C "$fox_tmp" || die "could not unpack Node"
  mv "$fox_tmp/$(fox "$fox_build.node.folder")" "$fox_tmp/node"
  mkdir "$fox_tmp/runtime"
  cp -R "$fox_package/package.json" "$fox_package/package-lock.json" "$fox_package/sqlite" "$fox_tmp/runtime/"
  (cd "$fox_tmp/runtime" && env -i PATH="$fox_tmp/node/bin:/usr/bin:/bin" HOME="$fox_tmp" \
    npm_config_cache="$fox_tmp/npm-cache" npm_config_update_notifier=false \
    "$fox_tmp/node/bin/node" "$fox_tmp/node/lib/node_modules/npm/bin/npm-cli.js" \
    ci --ignore-scripts --omit=dev --no-audit --no-fund >&2) || die "npm ci of the Camoufox runtime failed"
  for part in camoufox node runtime; do
    rm -rf "${fox_dir:?}/$part"
    mv "$fox_tmp/$part" "$fox_dir/$part"
  done
  cp "$fox_pin" "$fox_dir/manifest.json"
  say "installed Camoufox $fox_version to $fox_dir"
}

for engine in $engines; do
  "$engine"
done
