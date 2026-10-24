#!/bin/sh
# Installs a latch binary you already have into ~/.local/bin, or --bin-dir.
#
#   sh install-binary.sh [--bin-dir DIR] [--state-dir DIR] [--browser chrome|camoufox]... [--no-browser] [--client-only] [--non-interactive] [path/to/latch]
#
# A first install (no login in the state directory yet) at a terminal ends
# by running `latch setup` (log in, connect Google, and so on), or, when it
# installs Plow Latch.app on macOS 26 or later, by opening the app, whose
# window does the same; --non-interactive, or no terminal, leaves that for
# later. An upgrade never runs either: setup could start a host the upgrade
# did not mean to. On macOS it refuses anything before macOS 13.
#
# With no binary path, installs the latch next to this script. Options can
# appear before or after the path. --help prints usage without installing.
#
# A host on the state directory runs the latch being replaced: when the
# version changes, the new latch stops it (`latch stop`) first. One
# `serve -d` started is started again with the new latch, with what it was
# last given (`serve -d --again`), once everything is in place. One that
# runs in a terminal (an attached `latch serve`, or `serve --foreground`)
# cannot be started again in that terminal: the install says to run `latch
# serve` there again. A host the new latch cannot reach (one from an older
# protocol) is left for its owner to stop; the install says how, and goes
# on.
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
# Once the new versions are in, it removes the ones this latch does not
# run (an older latch's gog, reader and browser engines), unless a host it
# could not stop still runs.
#
# It also installs a browser engine for `latch browser`, with
# browser-engines.sh next to this script: Chrome for Testing by default (on
# macOS and Linux arm64/x86_64), or the engines `--browser` names (chrome,
# camoufox; repeat it for both). `--no-browser` installs none;
# `latch browser setup --engine ENGINE` installs one later.
#
# On Linux it also installs bubblewrap, which `latch serve` uses to sandbox
# its workers, and on Ubuntu 24.04 and later the AppArmor profile that lets
# bubblewrap create user namespaces there (browser-engines.sh adds Xvfb and
# unzip and runtime libraries for Chrome). Those need root, so it asks for sudo
# once, and only when something is missing. On macOS none of that is
# needed. It downloads no latch binary. Running it again changes
# nothing that is already in place.
#
# --client-only installs latch alone, for a machine that only asks other
# hosts, such as an agent's image: no gog, Messages reader, bubblewrap or
# browser engine, and no setup or app window at the end, only the word to
# run `latch login`.
set -eu

say() { printf 'install-binary.sh: %s\n' "$*"; }
die() { say "$*" >&2; exit 1; }
# One word for the shell, whatever it holds.
quote() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"; }

usage="usage: sh install-binary.sh [--bin-dir DIR] [--state-dir DIR] [--browser chrome|camoufox]... [--no-browser] [--client-only] [--non-interactive] [path/to/latch]"
# HOME=/ (a container's root) gives /.local/bin, not //.local/bin.
bin_dir="${HOME%/}/.local/bin"
state_dir=${LATCH_STATE_DIR:-"${HOME%/}/.latch"}
browsers=""
no_browser=""
client_only=""
non_interactive=""
state_given=${LATCH_STATE_DIR:+1}
src="$(dirname "$0")/latch"
given_src=""
while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help)
      printf '%s\n' "$usage" '' \
        'Without a binary path, installs the latch next to this script.' \
        'Options may appear before or after the binary path.' \
        'Binary directory defaults to ~/.local/bin; state and plugins default to $LATCH_STATE_DIR, else ~/.latch.' \
        'The browser engine defaults to Chrome for Testing; --browser chrome|camoufox' \
        'names the engines instead (repeat it for both), --no-browser installs none.' \
        '--client-only installs latch alone, for a machine that only asks hosts.'
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
      state_given=1
      shift 2
      ;;
    --browser)
      case "${2:-}" in
        chrome | camoufox) browsers="$browsers $2" ;;
        *) die "$usage" ;;
      esac
      shift 2
      ;;
    --no-browser)
      no_browser=1
      shift
      ;;
    --client-only)
      client_only=1
      shift
      ;;
    --non-interactive)
      non_interactive=1
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
if [ -n "$client_only" ]; then
  [ -z "$browsers" ] || die "--client-only installs no browser, so --browser can't be given"
elif [ -n "$no_browser" ]; then
  [ -z "$browsers" ] || die "--no-browser and --browser can't both be given"
elif [ -z "$browsers" ]; then
  browsers=chrome
fi
[ -f "$src" ] && [ -x "$src" ] || die "$src is not an executable file"

dest="$bin_dir/latch"
# On macOS, the app bundle the binary (or the link to it) sits in, if any.
app=""
if [ "$(uname -s)" = Darwin ]; then
  macos_version=$(sw_vers -productVersion)
  macos=${macos_version%%.*}
  [ "$macos" -ge 13 ] || die "latch needs macOS 13 or later, and this Mac runs macOS $macos_version; nothing was installed"
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
# A folder this user can't write, such as /.local/bin for a user whose HOME
# is /, is refused before anything changes.
if ! { mkdir -p "$bin_dir" && [ -w "$bin_dir" ]; } 2>/dev/null; then
  where="choose a folder you can write to with --bin-dir DIR"
  [ -n "$state_given" ] || where="$where, and set \`LATCH_STATE_DIR\` to a writable state folder"
  die "can't write to $bin_dir; $where; nothing was installed"
fi
# The latch being replaced: on macOS the app's own, wherever a link to it
# is (or none is), since the app is what gets replaced; elsewhere $dest.
old=$dest
if [ -n "$app" ]; then
  old="$HOME/Applications/$(basename "$app")/Contents/MacOS/$(basename "$inner")"
fi
# A first install: no login in the state directory yet.
first_install=""
[ -f "$state_dir/config.json" ] || first_install=1
# Whether the install replaces the latch there. On macOS the whole app
# counts: a new window with the same latch is a new app. cksum, unlike cmp,
# is on every system.
replace=1
if [ -n "$app" ]; then
  installed="$HOME/Applications/$(basename "$app")"
  if [ -d "$installed" ] && diff -rq "$app" "$installed" >/dev/null 2>&1; then
    replace=""
  fi
elif [ -f "$dest" ] && [ ! -L "$dest" ] && [ "$(cksum < "$src")" = "$(cksum < "$dest")" ]; then
  replace=""
fi

# 0. A host running the latch being replaced is stopped first; only a stop
# it confirms counts, and a failed one changes nothing. If anything fails
# before one `serve -d` started runs again, show the command for one
# explicit start.
restart_host=""
terminal_host=""
tmp=""
on_exit() {
  ended=$?
  rm -rf ${tmp:+"$tmp"}
  if [ "$ended" -ne 0 ] && [ -n "$restart_host" ]; then
    say "the host on $state_dir is stopped; start once with: $restart_host (plus any --deny-exec or --headless-browsers flags you need)" >&2
  fi
}
trap on_exit EXIT
# The new latch asks and stops: the latch there may be too old to know
# `stop`, or to reach a host of a latch older still.
host=""
if [ -x "$old" ] && [ -n "$replace" ]; then
  host=$("$src" --state-dir "$state_dir" status --local 2>/dev/null) || host=""
  case "$host" in
    # A host this latch cannot reach: its owner is told how to stop it,
    # and the install goes on.
    *'"other_version"'*)
      say "$(printf '%s\n' "$host" | sed -n 's/.*"blocked":{"code":"other_version","message":"\([^"]*\)"}.*/\1/p')"
      host=""
      ;;
  esac
fi
case "$host" in
  *'"running":true'*)
    stopped=$("$src" --state-dir "$state_dir" stop) || die "could not stop the host on $state_dir (see above); nothing was changed"
    case "$stopped" in
      *'"stopped":true'*)
        case "$host" in
          *'"attached":true'*) terminal_host=1 ;;
          *'"managed":true'*) restart_host="$(quote "$dest") --state-dir $(quote "$state_dir") serve -d" ;;
          *) terminal_host=1 ;;
        esac
        if [ -n "$terminal_host" ]; then
          say "stopped the host on $state_dir, which ran in a terminal; run \`latch serve\` again to start it with the new latch"
        else
          say "stopped the host on $state_dir; it starts again with the new latch once the install is done"
        fi
        ;;
    esac
    ;;
esac

# 1. The binary. Copy next to the target, then rename, so a running latch is
# never overwritten in place.
if [ -n "$app" ]; then
  if [ -z "$replace" ]; then
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
elif [ -z "$replace" ]; then
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

# 2. The pinned gog, which only a host runs. The pin is JSON, but its shape
# is fixed: one version string and a list of sha256 hashes, one for each OS
# and architecture.
if [ -z "$client_only" ]; then
  pin="$(dirname "$0")/src/google/pin.json"
  [ -f "$pin" ] || die "$pin not found; run install-binary.sh from the latch workspace"
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
    curl -fsSL --proto '=https' -o "$tmp/gog.tar.gz" "$url" || die "could not download $url"
    # Only the one file, written by this script, so the archive chooses no
    # path, owner or mode.
    tar -xzOf "$tmp/gog.tar.gz" ./gog > "$tmp/gog" || die "$url has no gog in it"
    pinned "$tmp/gog" || die "the gog in $url has sha256 $(sha256 "$tmp/gog"), which $pin does not list; nothing installed"
    chmod 755 "$tmp/gog"
    mv "$tmp/gog" "$gog"
    say "installed gog $gog_version to $gog (sha256 $(sha256 "$gog"))"
  fi
fi

# 3. macOS: the Messages reader packaged beside this latch, if there is one,
# in a folder named for this latch's version (the one that runs it).
reader="$(dirname "$src")/latch-messages"
if [ "$(uname -s)" = Darwin ] && [ -f "$reader" ] && [ -z "$client_only" ]; then
  # `--version` says {"version":"0.1.0 (1327): commit …"}: its first word.
  latch_version=$("$dest" --version | sed -n 's/.*"version": *"\([^" ]*\).*/\1/p')
  [ -n "$latch_version" ] || die "could not read $dest's version"
  reader_dir="$state_dir/plugins/messages/$latch_version"
  mkdir -p "$reader_dir"
  cp "$reader" "$reader_dir/latch-messages.tmp"
  chmod 755 "$reader_dir/latch-messages.tmp"
  mv "$reader_dir/latch-messages.tmp" "$reader_dir/latch-messages"
  say "installed the Messages reader to $reader_dir/latch-messages"
fi

# 4. Linux hosts: work out what needs root, then run it all under one sudo.
# What Chrome needs is browser-engines.sh's to install, then or later.
if [ "$(uname -s)" = Linux ] && [ -z "$client_only" ]; then
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
    . "$(dirname "$0")/install-packages.sh"
    if [ -n "$apparmor" ]; then
      as_root_install --then "$apparmor" "loading the bwrap-userns-restrict AppArmor profile" $packages
    else
      as_root_install $packages
    fi
  fi
fi

# 5. The browser engines.
if [ -n "$browsers" ]; then
  sh "$(dirname "$0")/browser-engines.sh" --state-dir "$state_dir" $browsers
fi

# 6. The helper versions an older latch installed and this one does not
# run. The host was stopped in step 0; one this latch could not stop keeps
# them, and removes them itself once it starts again on this latch.
if [ -z "$client_only" ]; then
  "$dest" --state-dir "$state_dir" prune-helpers > /dev/null ||
    say "could not remove older helper versions from $state_dir/plugins (see above)"
fi

# 7. The host stopped in step 0, started again by the new latch.
if [ -n "$restart_host" ]; then
  "$dest" --state-dir "$state_dir" serve -d --again > /dev/null ||
    die "the new latch did not start the host again (see above)"
  restart_host=""
  say "started the host on $state_dir again with the new latch"
fi
if [ -n "$terminal_host" ]; then
  say "the host on $state_dir ran in a terminal and was stopped: run \`latch serve\` there again to start it with the new latch"
fi

# On a first install at a terminal, `latch setup` takes it from here,
# reading the terminal itself (this script's own stdin may be the download
# it came from), or, on macOS 26 or later, the installed app's window. A
# state directory given here goes to the app as an absolute path, since it
# runs from another folder. Whatever happens in setup, the install is done.
if [ -n "$first_install" ]; then
  if [ -n "$client_only" ]; then
    say "next: run \`latch login\` to log in"
  elif [ -n "$non_interactive" ] || [ ! -t 1 ] || ! (: </dev/tty) 2>/dev/null; then
    say "next: run \`latch setup\` to log in and set up the rest"
  elif [ -n "$app" ] && [ "$macos" -ge 26 ]; then
    say "opening $installed to set up the rest"
    if [ -n "$state_given" ]; then
      set -- "$installed" --args --state-dir "$(cd "$state_dir" && pwd)"
    else
      set -- "$installed"
    fi
    open "$@" || say "could not open $installed; run \`latch setup\` instead"
  else
    say "running latch setup"
    if [ -n "$state_given" ]; then
      set -- --state-dir "$state_dir" setup
    else
      set -- setup
    fi
    "$dest" "$@" </dev/tty || say "setup did not finish; run \`latch setup\` any time"
  fi
fi
say "done"
