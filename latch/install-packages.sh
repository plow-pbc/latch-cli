# Sourced by install-binary.sh and browser-engines.sh, which define say and
# die: installs system packages as root, under one sudo.
#
#   as_root_install [--then STEP NOTE] PACKAGE...
#
# PACKAGE is named as apt calls it; engine libraries and Xvfb are mapped
# to the other package managers' names. STEP, a shell command, runs as root after the packages, under
# the same sudo, and NOTE says what it does.
as_root_install() {
  then_step=""
  then_note=""
  if [ "$1" = --then ]; then
    then_step=$2
    then_note=$3
    shift 3
  fi
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
  named=""
  for package in "$@"; do
    case "$manager:$package" in
      apt-get:libgtk-3-0 | apt-get:libasound2)
        if apt-cache show "${package}t64" >/dev/null 2>&1; then package="${package}t64"; fi
        ;;
      dnf:libgtk-3-0) package=gtk3 ;;
      dnf:libasound2) package=alsa-lib ;;
      dnf:libnss3) package=nss ;;
      dnf:libgbm1) package=mesa-libgbm ;;
      dnf:libx11-xcb1) package=libX11-xcb ;;
      dnf:libdbus-1-3) package=dbus-libs ;;
      pacman:libgtk-3-0) package=gtk3 ;;
      pacman:libasound2) package=alsa-lib ;;
      pacman:libnss3) package=nss ;;
      pacman:libgbm1) package=mesa ;;
      pacman:libx11-xcb1) package=libx11 ;;
      pacman:libdbus-1-3) package=dbus ;;
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
  say "installing as root: $named${then_note:+ (and $then_note)}"
  if [ "$(id -u)" -eq 0 ]; then
    as_root=""
  elif command -v sudo >/dev/null 2>&1; then
    as_root=sudo
  else
    die "need root to install $named, and sudo is not installed; run this as root"
  fi
  $as_root sh -c "$install${then_step:+ && $then_step}" || die "installing as root failed (see above); fix that and run this again"
}
