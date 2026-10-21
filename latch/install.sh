#!/bin/sh
# Installs a latch binary you already have into ~/.local/bin, or --bin-dir.
#
#   sh install.sh [--bin-dir DIR] [--state-dir DIR] [path/to/latch]
#
# With no binary path, installs the latch next to this script. Options can
# appear before or after the path. --help prints usage without installing.
#
# It also installs gog, the Google CLI a host runs for `latch google`: the
# release pinned in src/google/pin.json next to this script (the pin latch
# itself checks gog against), into the state directory's plugins folder,
# <state-dir>/plugins/gog/<version>/gog. The state directory is ~/.latch,
# latch's default, unless --state-dir names another (a dev install's). Each
# version gets its own folder, so a new pin never overwrites the old gog.
# That download needs curl and tar.
#
# On Linux it also installs bubblewrap, which `latch serve` uses to sandbox
# its workers, and on Ubuntu 24.04 and later the AppArmor profile that lets
# bubblewrap create user namespaces there. Those need root, so it asks for
# sudo once, and only when something is missing. On macOS none of that is
# needed. It downloads no latch binary. Running it again changes
# nothing that is already in place.
set -eu

say() { printf 'install.sh: %s\n' "$*"; }
die() { say "$*" >&2; exit 1; }

usage="usage: sh install.sh [--bin-dir DIR] [--state-dir DIR] [path/to/latch]"
bin_dir="$HOME/.local/bin"
state_dir="$HOME/.latch"
src="$(dirname "$0")/latch"
given_src=""
while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help)
      printf '%s\n' "$usage" '' \
        'Without a binary path, installs the latch next to this script.' \
        'Options may appear before or after the binary path.' \
        'Binary directory defaults to ~/.local/bin; state and plugins default to ~/.latch.'
      exit 0
      ;;
    --bin-dir)
      [ $# -ge 2 ] && [ -n "$2" ] || die "$usage"
      bin_dir=$2
      shift 2
      ;;
    --state-dir)
      [ $# -ge 2 ] && [ -n "$2" ] || die "$usage"
      state_dir=$2
      shift 2
      ;;
    -*) die "$usage" ;;
    *)
      [ -z "$given_src" ] || die "$usage"
      src=$1
      given_src=1
      shift
      ;;
  esac
done
[ -f "$src" ] && [ -x "$src" ] || die "$src is not an executable file"

# 1. The binary. Copy next to the target, then rename, so a running latch is
# never overwritten in place. cksum, unlike cmp, is on every system.
dest="$bin_dir/latch"
if [ -f "$dest" ] && [ "$(cksum < "$src")" = "$(cksum < "$dest")" ]; then
  say "latch already installed at $dest"
else
  mkdir -p "$bin_dir"
  cp "$src" "$dest.tmp"
  chmod 755 "$dest.tmp"
  mv "$dest.tmp" "$dest"
  say "installed latch to $dest"
fi
case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) say "add $bin_dir to your PATH to run latch by name" ;;
esac

# 2. The pinned gog. The pin is JSON, but its shape is fixed: one version
# string and a list of sha256 hashes, one for each OS and architecture.
pin="$(dirname "$0")/src/google/pin.json"
[ -f "$pin" ] || die "$pin not found; run install.sh from the latch workspace"
gog_version=$(sed -n 's/^ *"version": *"\([0-9][0-9.]*\)".*/\1/p' "$pin")
gog_hashes=$(tr -cs '0-9a-f' '\n' < "$pin" | grep -x '[0-9a-f]\{64\}' || true)
[ -n "$gog_version" ] && [ -n "$gog_hashes" ] || die "$pin has no gog version or hashes"
if command -v sha256sum >/dev/null 2>&1; then
  sha256() { sha256sum < "$1" | cut -d ' ' -f 1; }
elif command -v shasum >/dev/null 2>&1; then
  sha256() { shasum -a 256 < "$1" | cut -d ' ' -f 1; }
else
  die "need sha256sum or shasum to check gog"
fi
pinned() { printf '%s\n' "$gog_hashes" | grep -qx "$(sha256 "$1")"; }

gog_dir="$state_dir/plugins/gog/$gog_version"
gog="$gog_dir/gog"
if [ -f "$gog" ] && pinned "$gog"; then
  say "gog $gog_version already installed at $gog"
else
  case "$(uname -s)" in
    Darwin) os=darwin ;;
    Linux) os=linux ;;
    *) die "no gog release for $(uname -s)" ;;
  esac
  case "$(uname -m)" in
    arm64 | aarch64) arch=arm64 ;;
    x86_64 | amd64) arch=amd64 ;;
    *) die "no gog release for $(uname -m)" ;;
  esac
  for tool in curl tar; do
    command -v "$tool" >/dev/null 2>&1 || die "need $tool to download gog; install it, then run this again"
  done
  url="https://github.com/openclaw/gogcli/releases/download/v$gog_version/gogcli_${gog_version}_${os}_${arch}.tar.gz"
  # The state directory is this user's only, as latch keeps it.
  (umask 077 && mkdir -p "$state_dir")
  mkdir -p "$gog_dir"
  tmp=$(mktemp -d "$gog_dir/.download.XXXXXX")
  trap 'rm -rf ${tmp:+"$tmp"}' EXIT
  curl -fsSL --proto '=https' -o "$tmp/gog.tar.gz" "$url" || die "could not download $url"
  # Only the one file, written by this script, so the archive chooses no
  # path, owner or mode.
  tar -xzOf "$tmp/gog.tar.gz" ./gog > "$tmp/gog" || die "$url has no gog in it"
  pinned "$tmp/gog" || die "the gog in $url has sha256 $(sha256 "$tmp/gog"), which $pin does not list; nothing installed"
  chmod 755 "$tmp/gog"
  mv "$tmp/gog" "$gog"
  say "installed gog $gog_version to $gog (sha256 $(sha256 "$gog"))"
fi

# 3. Linux: work out what needs root, then run it all under one sudo.
if [ "$(uname -s)" = Linux ]; then
  packages=""
  want() { packages="${packages:+$packages }$1"; }
  command -v bwrap >/dev/null 2>&1 || want bubblewrap

  # Ubuntu 24.04 and later let a program create user namespaces only if an
  # AppArmor profile allows it (the kernel setting below reads 1). Ubuntu's
  # bwrap-userns-restrict profile allows it for /usr/bin/bwrap; 24.04 ships it
  # in apparmor-profiles, later releases install it already. Without AppArmor
  # (a container, WSL) the setting is absent and nothing is needed. The profile
  # is copied only after it loads, so a rerun can trust the copy.
  apparmor=""
  profile=/etc/apparmor.d/bwrap-userns-restrict
  restrict=/proc/sys/kernel/apparmor_restrict_unprivileged_userns
  if [ "$(cat "$restrict" 2>/dev/null)" = 1 ] && [ ! -f "$profile" ]; then
    want "apparmor apparmor-profiles"
    extra=/usr/share/apparmor/extra-profiles/bwrap-userns-restrict
    apparmor="apparmor_parser -r $extra && cp $extra $profile"
  fi

  if [ -z "$packages" ]; then
    say "bubblewrap already installed"
  else
    if command -v apt-get >/dev/null 2>&1; then
      manager=apt-get
    elif command -v dnf >/dev/null 2>&1; then
      manager=dnf
    elif command -v apk >/dev/null 2>&1; then
      manager=apk
    elif command -v pacman >/dev/null 2>&1; then
      manager=pacman
    else
      die "no apt-get, dnf, apk or pacman found; install $packages yourself, then run this again"
    fi
    named=$packages
    case $manager in
      apt-get) install="apt-get update -q && DEBIAN_FRONTEND=noninteractive apt-get install -y -q $named" ;;
      dnf) install="dnf install -y $named" ;;
      apk) install="apk add $named" ;;
      pacman) install="pacman -Sy --needed --noconfirm $named" ;;
    esac
    root_steps="$install${apparmor:+ && $apparmor}"

    say "installing as root: $named${apparmor:+ (and loading the bwrap-userns-restrict AppArmor profile)}"
    if [ "$(id -u)" -eq 0 ]; then
      as_root=""
    elif command -v sudo >/dev/null 2>&1; then
      as_root=sudo
    else
      die "need root for that, and sudo is not installed; run this as root"
    fi
    $as_root sh -c "$root_steps" || die "installing as root failed (see above); fix that and run this again"
  fi
fi

say "done"
