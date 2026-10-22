#!/bin/sh
# Installs a latch binary you already have into ~/.local/bin, or --bin-dir.
#
#   sh install.sh [--bin-dir DIR] [--state-dir DIR] [--with-camoufox] [path/to/latch]
#
# With no binary path, installs the latch next to this script. Options can
# appear before or after the path. --help prints usage without installing.
#
# On macOS a release's latch is inside Plow Latch.app (the `latch` beside
# this script leads into it). Then the whole app goes to ~/Applications,
# replacing an older one at the same path, so it keeps its identity and its
# Full Disk Access, and ~/.local/bin/latch becomes a link to the latch
# inside it.
#
# It also installs gog, the Google CLI a host runs for `latch google`: the
# release pinned in src/google/pin.json next to this script (the pin latch
# itself checks gog against), into the state directory's plugins folder,
# <state-dir>/plugins/gog/<version>/gog. The state directory is ~/.latch,
# latch's default, unless --state-dir names another (a dev install's). Each
# version gets its own folder, so a new pin never overwrites the old gog.
# That download needs curl and tar.
#
# On macOS it installs the Messages reader that came with this latch, if it
# did, into <state-dir>/plugins/messages/<latch version>/latch-messages; latch
# runs only the reader it was built with.
#
# On macOS and on Linux arm64 (the Pi) it also installs Chrome for Testing,
# the browser a host runs for `latch browser`: the release pinned in
# src/browser/cft.json, into <state-dir>/plugins/cft/<version>/, beside older
# versions, with the pin copied in as manifest.json. That download needs
# curl, and ditto (macOS) or unzip (Linux).
#
# With --with-camoufox (macOS), it also installs Camoufox, the second
# browser engine, pinned in src/browser/camoufox.json: the Camoufox build,
# the Node it runs its helper on, and the helper's runtime (Playwright and
# camoufox-js, from camoufox/'s lockfile, with `npm ci`), into
# <state-dir>/plugins/camoufox/<version>/. About 1 GB on disk.
#
# On Linux it also installs bubblewrap, which `latch serve` uses to sandbox
# its workers, and on Ubuntu 24.04 and later the AppArmor profile that lets
# bubblewrap create user namespaces there; on arm64, also Xvfb, the display
# of a host with no screen, and unzip. Those need root, so it asks for sudo
# once, and only when something is missing. On macOS none of that is
# needed. It downloads no latch binary. Running it again changes
# nothing that is already in place.
set -eu

say() { printf 'install.sh: %s\n' "$*"; }
die() { say "$*" >&2; exit 1; }

usage="usage: sh install.sh [--bin-dir DIR] [--state-dir DIR] [--with-camoufox] [path/to/latch]"
bin_dir="$HOME/.local/bin"
state_dir="$HOME/.latch"
with_camoufox=""
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
    --with-camoufox)
      with_camoufox=1
      shift
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
# On macOS, the app bundle the binary (or the link to it) sits in, if any.
app=""
if [ "$(uname -s)" = Darwin ]; then
  inner=$src
  if [ -L "$src" ]; then
    case "$(readlink "$src")" in
      /*) inner=$(readlink "$src") ;;
      *) inner="$(dirname "$src")/$(readlink "$src")" ;;
    esac
  fi
  case "$inner" in
    *.app/Contents/MacOS/*) app=${inner%/Contents/MacOS/*} ;;
  esac
fi
if [ -n "$app" ]; then
  installed="$HOME/Applications/$(basename "$app")"
  if [ -d "$installed" ] && [ "$(cksum < "$inner")" = "$(cksum < "$installed/Contents/MacOS/$(basename "$inner")")" ]; then
    say "$(basename "$app") already installed at $installed"
  else
    # Copied beside the old one first. The old one then moves aside, not
    # away, until the new one has its path: if that fails, the old one goes
    # back, still working, and the link into it with it. A run cut short
    # between the two leaves the old one aside, which the next run restores.
    mkdir -p "$HOME/Applications"
    if [ ! -d "$installed" ] && [ -d "$installed.old" ]; then
      mv "$installed.old" "$installed"
    fi
    rm -rf "$installed.new" "$installed.old"
    cp -R "$app" "$installed.new"
    if [ -d "$installed" ] && ! mv "$installed" "$installed.old"; then
      rm -rf "$installed.new"
      die "could not move $installed aside to update it; it is unchanged"
    fi
    if ! mv "$installed.new" "$installed"; then
      if [ -d "$installed.old" ]; then
        mv "$installed.old" "$installed"
      fi
      rm -rf "$installed.new"
      die "could not put the new $(basename "$app") in place; the old one is still at $installed"
    fi
    rm -rf "$installed.old"
    say "installed $(basename "$app") to $installed"
  fi
  link="$installed/Contents/MacOS/$(basename "$inner")"
  if [ -L "$dest" ] && [ "$(readlink "$dest")" = "$link" ]; then
    say "latch already linked at $dest"
  else
    mkdir -p "$bin_dir"
    ln -sf "$link" "$dest.tmp"
    mv -f "$dest.tmp" "$dest"
    say "linked $dest to $link"
  fi
elif [ -f "$dest" ] && [ ! -L "$dest" ] && [ "$(cksum < "$src")" = "$(cksum < "$dest")" ]; then
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
  trap 'rm -rf ${tmp:+"$tmp"} ${cft_tmp:+"$cft_tmp"} ${fox_tmp:+"$fox_tmp"}' EXIT
  curl -fsSL --proto '=https' -o "$tmp/gog.tar.gz" "$url" || die "could not download $url"
  # Only the one file, written by this script, so the archive chooses no
  # path, owner or mode.
  tar -xzOf "$tmp/gog.tar.gz" ./gog > "$tmp/gog" || die "$url has no gog in it"
  pinned "$tmp/gog" || die "the gog in $url has sha256 $(sha256 "$tmp/gog"), which $pin does not list; nothing installed"
  chmod 755 "$tmp/gog"
  mv "$tmp/gog" "$gog"
  say "installed gog $gog_version to $gog (sha256 $(sha256 "$gog"))"
fi

# 3. macOS: the Messages reader packaged beside this latch, if there is one,
# in a folder named for this latch's version (the one that runs it).
reader="$(dirname "$src")/latch-messages"
if [ "$(uname -s)" = Darwin ] && [ -f "$reader" ]; then
  latch_version=$("$dest" --version | awk '{print $2}')
  [ -n "$latch_version" ] || die "could not read $dest's version"
  reader_dir="$state_dir/plugins/messages/$latch_version"
  mkdir -p "$reader_dir"
  cp "$reader" "$reader_dir/latch-messages.tmp"
  chmod 755 "$reader_dir/latch-messages.tmp"
  mv "$reader_dir/latch-messages.tmp" "$reader_dir/latch-messages"
  say "installed the Messages reader to $reader_dir/latch-messages"
fi

# 4. Linux: work out what needs root, then run it all under one sudo.
# On a Linux with a Chrome for Testing build (arm64, the Pi), the browser
# also needs Xvfb, a screenless host's display, and unzip to unpack it.
cft_linux=""
case "$(uname -s)/$(uname -m)" in
  Linux/aarch64 | Linux/arm64) cft_linux=linux_arm64 ;;
esac
if [ "$(uname -s)" = Linux ]; then
  packages=""
  want() { packages="${packages:+$packages }$1"; }
  command -v bwrap >/dev/null 2>&1 || want bubblewrap
  if [ -n "$cft_linux" ]; then
    command -v Xvfb >/dev/null 2>&1 || want xvfb
    command -v unzip >/dev/null 2>&1 || want unzip
  fi

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
    say "bubblewrap${cft_linux:+, Xvfb and unzip} already installed"
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
    # Xvfb's package has a different name on each.
    named=""
    for package in $packages; do
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

# 5. The pinned Chrome for Testing, on macOS and on Linux arm64. The pin's
# JSON has a fixed shape: each build is one block of "key": "value" lines,
# which sed reads. install.sh checks the zip's sha256; latch hashes the
# whole unpacked build before every launch. The manifest goes in last, so a
# folder without it is a half-made install, made again.
case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) cft_build=mac_arm64 ;;
  Darwin/x86_64) cft_build=mac_x64 ;;
  Darwin/*) die "no Chrome for Testing build for $(uname -m)" ;;
  *) cft_build=$cft_linux ;;
esac
if [ -z "$cft_build" ]; then
  say "no Chrome for Testing build for $(uname -s) $(uname -m): this host runs no browser"
else
  cft_pin="$(dirname "$0")/src/browser/cft.json"
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
  else
    command -v curl >/dev/null 2>&1 || die "need curl to download Chrome for Testing"
    cft_url=$(cft url)
    cft_zip_sha256=$(cft zip_sha256)
    cft_folder=$(cft folder)
    (umask 077 && mkdir -p "$state_dir")
    mkdir -p "$cft_dir"
    cft_tmp=$(mktemp -d "$cft_dir/.download.XXXXXX")
    trap 'rm -rf ${tmp:+"$tmp"} ${cft_tmp:+"$cft_tmp"} ${fox_tmp:+"$fox_tmp"}' EXIT
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
  fi
fi

# 6. macOS, with --with-camoufox: the pinned Camoufox, Node and the helper's
# runtime, each checked as it arrives (the archives' sha256, `npm ci`'s
# lockfile integrity); latch hashes each unpacked part before every launch.
# No package's install script runs. The manifest goes in last.
if [ -n "$with_camoufox" ]; then
  [ "$(uname -s)" = Darwin ] || die "--with-camoufox: Camoufox is pinned for macOS hosts only"
  fox_pin="$(dirname "$0")/src/browser/camoufox.json"
  fox_package="$(dirname "$0")/camoufox"
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
  else
    command -v curl >/dev/null 2>&1 || die "need curl to download Camoufox"
    (umask 077 && mkdir -p "$state_dir")
    mkdir -p "$fox_dir"
    # Absolute: npm runs from inside it.
    fox_tmp=$(cd "$(mktemp -d "$fox_dir/.download.XXXXXX")" && pwd)
    trap 'rm -rf ${tmp:+"$tmp"} ${cft_tmp:+"$cft_tmp"} ${fox_tmp:+"$fox_tmp"}' EXIT
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
  fi
fi

say "done"
