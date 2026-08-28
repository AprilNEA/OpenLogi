#!/bin/sh
# Install an OpenLogi Linux release package, or install a local release build
# from a repository checkout. This script intentionally uses POSIX /bin/sh.

set -eu

REPOSITORY=AprilNEA/OpenLogi
GITHUB_URL=https://github.com
RELEASES_URL="${GITHUB_URL}/${REPOSITORY}/releases"
# Public trust anchor for release package signatures. Keep this in sync with
# OPENLOGI_UPDATE_MINISIGN_PUBLIC_KEY in the release publishing configuration.
MINISIGN_PUBLIC_KEY=RWRRkFtw+rqkvTlCTGKUszSE5dX9CK1teaQD45jO4P9rYlWLO4/nHVUF

MODE=release
VERSION=latest
VERSION_SET=0
PACKAGE_MANAGER=
PACKAGE_MANAGER_SET=0
PREFIX=/usr/local
PREFIX_SET=0
NO_START=0
DRY_RUN=0
TEMP_DIR=

die() {
  printf 'Error: %s\n' "$*" >&2
  exit 1
}

cleanup() {
  if [ -n "$TEMP_DIR" ] && [ -d "$TEMP_DIR" ]; then
    rm -rf "$TEMP_DIR"
  fi
}

trap cleanup 0
trap 'exit 1' HUP INT TERM

usage() {
  cat <<EOF
Usage:
  $0 [OPTIONS]
  $0 --from-source [--prefix PREFIX] [OPTIONS]

Install the latest OpenLogi release package for this Linux distribution.
Release packages are downloaded from GitHub, authenticated with the embedded
minisign public key, and verified against the exact entry in SHA256SUMS before
the package manager is invoked with sudo. The minisign command is required.
Run this script as your normal user, not with sudo.

Options:
  --version VERSION          Install a fixed version (for example 0.6.0 or v0.6.0)
  --package-manager MANAGER  Override detection: apt, dnf, yum, zypper, rpm, pacman
  --no-start                 Do not enable or start the systemd user agent
  --dry-run                  Download and verify, but do not install or start
  --from-source              Install target/release binaries and repository resources
  --prefix PREFIX            Binary prefix for --from-source only (default: /usr/local)
  --help, -h                 Show this help

Examples:
  $0
  $0 --version 0.6.0 --package-manager apt
  $0 --from-source --prefix /usr
EOF
}

need_value() {
  [ "$#" -ge 2 ] || die "$1 requires a value"
  [ -n "$2" ] || die "$1 requires a non-empty value"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      need_value "$@"
      VERSION=$2
      VERSION_SET=1
      shift 2
      ;;
    --version=*)
      VERSION=${1#--version=}
      [ -n "$VERSION" ] || die "--version requires a non-empty value"
      VERSION_SET=1
      shift
      ;;
    --package-manager)
      need_value "$@"
      PACKAGE_MANAGER=$2
      PACKAGE_MANAGER_SET=1
      shift 2
      ;;
    --package-manager=*)
      PACKAGE_MANAGER=${1#--package-manager=}
      [ -n "$PACKAGE_MANAGER" ] || die "--package-manager requires a non-empty value"
      PACKAGE_MANAGER_SET=1
      shift
      ;;
    --no-start)
      NO_START=1
      shift
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    --from-source)
      MODE=source
      shift
      ;;
    --prefix)
      need_value "$@"
      PREFIX=$2
      PREFIX_SET=1
      shift 2
      ;;
    --prefix=*)
      PREFIX=${1#--prefix=}
      [ -n "$PREFIX" ] || die "--prefix requires a non-empty value"
      PREFIX_SET=1
      shift
      ;;
    --help | -h)
      usage
      exit 0
      ;;
    --)
      shift
      [ "$#" -eq 0 ] || die "positional arguments are not supported"
      ;;
    *) die "unknown argument: $1" ;;
  esac
done

need_command() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

require_linux() {
  need_command uname
  [ "$(uname -s)" = Linux ] || die "this installer supports Linux only"
}

make_temp_dir() {
  [ -n "$TEMP_DIR" ] && return
  need_command mktemp
  temp_base=${TMPDIR:-/tmp}
  [ -d "$temp_base" ] || die "temporary directory does not exist: $temp_base"
  umask 077
  TEMP_DIR=$(mktemp -d "${temp_base%/}/openlogi-install.XXXXXXXXXX") ||
    die "could not create a temporary directory"
}

validate_version() {
  candidate=${1#v}
  [ -n "$candidate" ] || return 1
  printf '%s\n' "$candidate" |
    grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z]+([.-][0-9A-Za-z]+)*)?$'
}

validate_prefix() {
  case "$PREFIX" in
    /*) ;;
    *) die "--prefix must be an absolute path" ;;
  esac
  case "$PREFIX" in
    *[!A-Za-z0-9_./+-]*) die "--prefix contains unsupported characters" ;;
  esac
  case "/${PREFIX#/}/" in
    *//* | */./* | */../*) die "--prefix must not contain empty, . or .. components" ;;
  esac
  if [ "$PREFIX" != / ]; then
    PREFIX=${PREFIX%/}
  fi
}

curl_download() {
  output=$1
  url=$2
  curl --proto '=https' --proto-redir '=https' --tlsv1.2 \
    --fail --location --silent --show-error \
    --retry 3 --retry-delay 1 --retry-connrefused \
    --output "$output" "$url"
}

latest_release_url() {
  curl --proto '=https' --proto-redir '=https' --tlsv1.2 \
    --fail --location --silent --show-error \
    --retry 3 --retry-delay 1 --retry-connrefused \
    --output /dev/null --write-out '%{url_effective}' "${RELEASES_URL}/latest"
}

resolve_version() {
  if [ "$VERSION" = latest ]; then
    effective_url=$(latest_release_url) || die "could not resolve the latest GitHub release"
    case "$effective_url" in
      "${RELEASES_URL}/tag/"*) release_tag=${effective_url#"${RELEASES_URL}/tag/"} ;;
      *) die "GitHub latest release redirected to an unexpected URL: $effective_url" ;;
    esac
    validate_version "$release_tag" || die "GitHub returned an invalid release tag: $release_tag"
    VERSION=${release_tag#v}
  else
    validate_version "$VERSION" || die "invalid version: $VERSION"
    VERSION=${VERSION#v}
  fi
  RELEASE_TAG=v${VERSION}
}

detect_architecture() {
  case "$(uname -m)" in
    x86_64 | amd64) PACKAGE_ARCH=amd64 ;;
    aarch64 | arm64) PACKAGE_ARCH=arm64 ;;
    *) die "unsupported Linux architecture: $(uname -m)" ;;
  esac
}

select_package_manager() {
  if [ "$PACKAGE_MANAGER_SET" -eq 0 ]; then
    if command -v apt-get >/dev/null 2>&1 || command -v apt >/dev/null 2>&1; then
      PACKAGE_MANAGER=apt
    elif command -v dnf >/dev/null 2>&1; then
      PACKAGE_MANAGER=dnf
    elif command -v yum >/dev/null 2>&1; then
      PACKAGE_MANAGER=yum
    elif command -v zypper >/dev/null 2>&1; then
      PACKAGE_MANAGER=zypper
    elif command -v pacman >/dev/null 2>&1; then
      PACKAGE_MANAGER=pacman
    elif command -v rpm >/dev/null 2>&1; then
      PACKAGE_MANAGER=rpm
    else
      die "could not detect apt, dnf, yum, zypper, pacman or rpm"
    fi
  fi

  case "$PACKAGE_MANAGER" in
    apt)
      PACKAGE_EXTENSION=deb
      if command -v apt-get >/dev/null 2>&1; then
        PACKAGE_COMMAND=apt-get
      elif command -v apt >/dev/null 2>&1; then
        PACKAGE_COMMAND=apt
      else
        die "apt was selected but neither apt-get nor apt is available"
      fi
      ;;
    dnf | yum | zypper | rpm)
      PACKAGE_EXTENSION=rpm
      PACKAGE_COMMAND=$PACKAGE_MANAGER
      need_command "$PACKAGE_COMMAND"
      ;;
    pacman)
      PACKAGE_EXTENSION=pkg.tar.zst
      PACKAGE_COMMAND=pacman
      need_command "$PACKAGE_COMMAND"
      ;;
    *) die "unsupported package manager: $PACKAGE_MANAGER" ;;
  esac
}

run_privileged() {
  if [ "$(id -u)" -eq 0 ]; then
    "$@"
  else
    need_command sudo
    sudo "$@"
  fi
}

print_command() {
  printf 'Would run:'
  for argument; do
    printf ' %s' "$argument"
  done
  printf '\n'
}

install_release_package() {
  case "$PACKAGE_MANAGER" in
    apt) set -- "$PACKAGE_COMMAND" install -y "$PACKAGE_PATH" ;;
    dnf | yum) set -- "$PACKAGE_COMMAND" install -y "$PACKAGE_PATH" ;;
    zypper) set -- zypper --non-interactive install --allow-unsigned-rpm "$PACKAGE_PATH" ;;
    rpm) set -- rpm -Uvh "$PACKAGE_PATH" ;;
    pacman) set -- pacman -U --noconfirm "$PACKAGE_PATH" ;;
  esac

  if [ "$DRY_RUN" -eq 1 ]; then
    print_command sudo "$@"
  else
    run_privileged "$@"
  fi
}

start_agent() {
  [ "$NO_START" -eq 0 ] || return 0
  if [ "$DRY_RUN" -eq 1 ]; then
    print_command systemctl --user enable --now openlogi-agent.service
    return 0
  fi
  if ! command -v systemctl >/dev/null 2>&1; then
    printf '%s\n' "systemctl is unavailable; the OpenLogi agent was not started." >&2
    return 0
  fi

  systemctl --user daemon-reload >/dev/null 2>&1 || true
  if systemctl --user enable --now openlogi-agent.service; then
    printf '%s\n' "Enabled and started openlogi-agent.service for the current user."
  else
    printf '%s\n' "OpenLogi installed, but the user agent could not be started." >&2
    printf '%s\n' "Try: systemctl --user enable --now openlogi-agent.service" >&2
  fi
}

install_release() {
  require_linux
  need_command curl
  need_command grep
  need_command awk
  need_command minisign
  need_command sha256sum
  need_command id

  resolve_version
  detect_architecture
  select_package_manager
  make_temp_dir

  PACKAGE_NAME="openlogi-${RELEASE_TAG}-linux-${PACKAGE_ARCH}.${PACKAGE_EXTENSION}"
  PACKAGE_PATH="${TEMP_DIR}/${PACKAGE_NAME}"
  SIGNATURE_PATH="${PACKAGE_PATH}.minisig"
  CHECKSUMS_PATH="${TEMP_DIR}/SHA256SUMS"
  CHECK_PATH="${TEMP_DIR}/SHA256SUMS.selected"
  RELEASE_DOWNLOAD_URL="${RELEASES_URL}/download/${RELEASE_TAG}"

  printf 'Downloading OpenLogi %s for %s (%s)...\n' \
    "$VERSION" "$PACKAGE_ARCH" "$PACKAGE_MANAGER"
  curl_download "$PACKAGE_PATH" "${RELEASE_DOWNLOAD_URL}/${PACKAGE_NAME}"
  curl_download "$SIGNATURE_PATH" "${RELEASE_DOWNLOAD_URL}/${PACKAGE_NAME}.minisig"
  curl_download "$CHECKSUMS_PATH" "${RELEASE_DOWNLOAD_URL}/SHA256SUMS"

  minisign -V -P "$MINISIGN_PUBLIC_KEY" -m "$PACKAGE_PATH" -x "$SIGNATURE_PATH" ||
    die "signature verification failed for $PACKAGE_NAME"

  awk -v file="$PACKAGE_NAME" '
    $2 == file && $1 ~ /^[0-9A-Fa-f]+$/ && length($1) == 64 {
      print tolower($1) "  " file
      matches++
    }
    END { if (matches != 1) exit 1 }
  ' "$CHECKSUMS_PATH" >"$CHECK_PATH" ||
    die "SHA256SUMS does not contain exactly one valid entry for $PACKAGE_NAME"

  (cd "$TEMP_DIR" && sha256sum -c "${CHECK_PATH##*/}") ||
    die "checksum verification failed for $PACKAGE_NAME"
  printf 'Authenticated and verified %s before privilege escalation.\n' "$PACKAGE_NAME"

  install_release_package
  start_agent

  if [ "$DRY_RUN" -eq 1 ]; then
    printf '%s\n' "Dry run complete; no package was installed."
  else
    printf '%s\n' "OpenLogi ${VERSION} installed. Run 'openlogi-desktop' to start the GUI."
  fi
}

install_source_file() {
  mode=$1
  source_path=$2
  destination=$3
  if [ "$DRY_RUN" -eq 1 ]; then
    print_command sudo install -D -m "$mode" "$source_path" "$destination"
  else
    run_privileged install -D -m "$mode" "$source_path" "$destination"
  fi
}

run_source_privileged() {
  if [ "$DRY_RUN" -eq 1 ]; then
    print_command sudo "$@"
  else
    run_privileged "$@"
  fi
}

install_source() {
  require_linux
  need_command id
  need_command install
  need_command sed
  validate_prefix

  SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P) ||
    die "could not locate the installer directory"
  REPO_ROOT=$(CDPATH='' cd "${SCRIPT_DIR}/../.." && pwd -P) ||
    die "--from-source must be run from a repository checkout"
  BUILD_DIR=${REPO_ROOT}/target/release

  for binary in openlogi openlogi-desktop openlogi-overlay openlogi-agent; do
    [ -x "${BUILD_DIR}/${binary}" ] ||
      die "${BUILD_DIR}/${binary} is missing; build all four release binaries first"
  done
  [ -f "${SCRIPT_DIR}/udev/70-openlogi.rules" ] || die "repository udev rules are missing"
  [ -f "${SCRIPT_DIR}/systemd/openlogi-agent.service" ] || die "repository systemd unit is missing"
  [ -f "${SCRIPT_DIR}/desktop/openlogi.desktop" ] || die "repository desktop entry is missing"
  [ -f "${REPO_ROOT}/design/icon/openlogi.png" ] || die "repository app icon is missing"

  if [ "$PREFIX" = / ]; then
    BINDIR=/bin
  else
    BINDIR=${PREFIX}/bin
  fi

  make_temp_dir
  escaped_bindir=$(printf '%s\n' "$BINDIR" | sed 's|[&\\|]|\\&|g')
  sed "s|^ExecStart=/usr/bin/openlogi-agent$|ExecStart=${escaped_bindir}/openlogi-agent|" \
    "${SCRIPT_DIR}/systemd/openlogi-agent.service" >"${TEMP_DIR}/openlogi-agent.service"

  printf 'Installing local release binaries to %s...\n' "$BINDIR"
  for binary in openlogi openlogi-desktop openlogi-overlay openlogi-agent; do
    install_source_file 755 "${BUILD_DIR}/${binary}" "${BINDIR}/${binary}"
  done

  install_source_file 644 "${SCRIPT_DIR}/udev/70-openlogi.rules" \
    /etc/udev/rules.d/70-openlogi.rules
  install_source_file 644 "${TEMP_DIR}/openlogi-agent.service" \
    /usr/lib/systemd/user/openlogi-agent.service
  install_source_file 644 "${SCRIPT_DIR}/desktop/openlogi.desktop" \
    /usr/share/applications/openlogi.desktop
  install_source_file 644 "${REPO_ROOT}/design/icon/openlogi.png" \
    /usr/share/icons/hicolor/1024x1024/apps/openlogi.png
  for size in 512 256 128 64 48 32 16; do
    sized_icon=${REPO_ROOT}/design/icon/openlogi-${size}.png
    [ -f "$sized_icon" ] || continue
    install_source_file 644 "$sized_icon" \
      "/usr/share/icons/hicolor/${size}x${size}/apps/openlogi.png"
  done

  if command -v udevadm >/dev/null 2>&1; then
    run_source_privileged udevadm control --reload-rules
    run_source_privileged udevadm trigger --subsystem-match=hidraw
    run_source_privileged udevadm trigger --subsystem-match=input
    run_source_privileged udevadm trigger --subsystem-match=misc --attr-match=name=uinput || true
    run_source_privileged udevadm settle || true
  fi
  if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    run_source_privileged gtk-update-icon-cache -qtf /usr/share/icons/hicolor || true
  fi
  if command -v update-desktop-database >/dev/null 2>&1; then
    run_source_privileged update-desktop-database -q /usr/share/applications || true
  fi

  start_agent
  if [ "$DRY_RUN" -eq 1 ]; then
    printf '%s\n' "Dry run complete; no files were installed."
  else
    printf '%s\n' "OpenLogi installed from target/release. Run 'openlogi-desktop' to start the GUI."
  fi
}

if [ "$MODE" = source ]; then
  [ "$(id -u)" -ne 0 ] || die "run this installer as a regular user; it elevates only the commands that need it"
  [ "$VERSION_SET" -eq 0 ] || die "--version cannot be used with --from-source"
  [ "$PACKAGE_MANAGER_SET" -eq 0 ] || die "--package-manager cannot be used with --from-source"
  install_source
else
  [ "$(id -u)" -ne 0 ] || die "run this installer as a regular user; it elevates only the package manager"
  [ "$PREFIX_SET" -eq 0 ] || die "--prefix applies only to --from-source"
  install_release
fi
