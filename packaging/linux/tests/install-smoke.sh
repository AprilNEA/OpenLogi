#!/bin/sh
# Mocked smoke tests for packaging/linux/install.sh. No network or system files
# are touched; curl, privilege escalation, package managers and lifecycle tools
# are replaced through PATH.

set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
REPO_ROOT=$(CDPATH='' cd "${SCRIPT_DIR}/../../.." && pwd -P)
INSTALLER=${REPO_ROOT}/packaging/linux/install.sh
ORIGINAL_PATH=$PATH
REAL_SHA256SUM=$(command -v sha256sum)
WORK_DIR=$(mktemp -d "${TMPDIR:-/tmp}/openlogi-install-smoke.XXXXXXXXXX")
MOCK_BIN=${WORK_DIR}/bin
MOCK_LOG=${WORK_DIR}/commands.log
MOCK_CURL_LOG=${WORK_DIR}/curl.log
MOCK_PACKAGE_PATH=${WORK_DIR}/package-path
MOCK_SERVICE_CAPTURE=${WORK_DIR}/openlogi-agent.service
OUTPUT=${WORK_DIR}/output

cleanup() {
  rm -rf "$WORK_DIR"
}
trap cleanup 0
trap 'exit 1' HUP INT TERM

mkdir -p "$MOCK_BIN"
: >"$MOCK_LOG"
: >"$MOCK_CURL_LOG"

export MOCK_LOG MOCK_CURL_LOG MOCK_PACKAGE_PATH MOCK_SERVICE_CAPTURE
export REAL_SHA256SUM
export MOCK_ARCH=x86_64
export MOCK_LATEST_TAG=v9.8.7
export PATH="${MOCK_BIN}:${ORIGINAL_PATH}"

cat >"${MOCK_BIN}/uname" <<'EOF'
#!/bin/sh
case "${1:-}" in
  -s) printf '%s\n' Linux ;;
  -m) printf '%s\n' "$MOCK_ARCH" ;;
  *) printf '%s\n' "unexpected uname arguments: $*" >&2; exit 1 ;;
esac
EOF

cat >"${MOCK_BIN}/id" <<'EOF'
#!/bin/sh
if [ "${1:-}" = -u ]; then
  printf '%s\n' "${MOCK_UID:-1000}"
else
  printf '%s\n' "unexpected id arguments: $*" >&2
  exit 1
fi
EOF

cat >"${MOCK_BIN}/sudo" <<'EOF'
#!/bin/sh
printf 'sudo %s\n' "$*" >>"$MOCK_LOG"
"$@"
EOF

cat >"${MOCK_BIN}/systemctl" <<'EOF'
#!/bin/sh
printf 'systemctl %s\n' "$*" >>"$MOCK_LOG"
EOF

cat >"${MOCK_BIN}/curl" <<'EOF'
#!/bin/sh
output=
url=
write_out=
printf 'curl %s\n' "$*" >>"$MOCK_CURL_LOG"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output)
      output=$2
      shift 2
      ;;
    --write-out)
      write_out=$2
      shift 2
      ;;
    https://*)
      url=$1
      shift
      ;;
    --proto | --proto-redir | --retry | --retry-delay)
      shift 2
      ;;
    *) shift ;;
  esac
done

[ -n "$url" ] || {
  printf '%s\n' "curl mock did not receive an HTTPS URL" >&2
  exit 1
}

case "$url" in
  https://github.com/AprilNEA/OpenLogi/releases/latest)
    [ "$write_out" = '%{url_effective}' ] || exit 1
    printf 'https://github.com/AprilNEA/OpenLogi/releases/tag/%s' "$MOCK_LATEST_TAG"
    ;;
  */SHA256SUMS)
    package_path=$(cat "$MOCK_PACKAGE_PATH")
    package_name=${package_path##*/}
    if [ "${MOCK_BAD_SUM:-0}" -eq 1 ]; then
      hash=0000000000000000000000000000000000000000000000000000000000000000
    else
      hash=$("$REAL_SHA256SUM" "$package_path")
      hash=${hash%% *}
    fi
    printf '%s  %s\n' "$hash" "$package_name" >"$output"
    ;;
  */openlogi-v*-linux-*.deb | */openlogi-v*-linux-*.rpm | */openlogi-v*-linux-*.pkg.tar.zst)
    printf '%s\n' fixture-package >"$output"
    printf '%s\n' "$output" >"$MOCK_PACKAGE_PATH"
    ;;
  *)
    printf '%s\n' "unexpected curl URL: $url" >&2
    exit 1
    ;;
esac
EOF

cat >"${MOCK_BIN}/package-manager" <<'EOF'
#!/bin/sh
printf '%s %s\n' "${0##*/}" "$*" >>"$MOCK_LOG"
EOF

for command in apt apt-get dnf yum zypper rpm pacman; do
  cp "${MOCK_BIN}/package-manager" "${MOCK_BIN}/${command}"
done

cat >"${MOCK_BIN}/install" <<'EOF'
#!/bin/sh
printf 'install %s\n' "$*" >>"$MOCK_LOG"
last=
previous=
for argument do
  previous=$last
  last=$argument
done
if [ "$last" = /usr/lib/systemd/user/openlogi-agent.service ]; then
  cp "$previous" "$MOCK_SERVICE_CAPTURE"
fi
EOF

cat >"${MOCK_BIN}/lifecycle-tool" <<'EOF'
#!/bin/sh
printf '%s %s\n' "${0##*/}" "$*" >>"$MOCK_LOG"
EOF

for command in udevadm gtk-update-icon-cache update-desktop-database; do
  cp "${MOCK_BIN}/lifecycle-tool" "${MOCK_BIN}/${command}"
done

chmod +x "${MOCK_BIN}"/*

reset_logs() {
  : >"$MOCK_LOG"
  : >"$MOCK_CURL_LOG"
  rm -f "$MOCK_PACKAGE_PATH" "$MOCK_SERVICE_CAPTURE" "$OUTPUT"
}

assert_contains() {
  file=$1
  expected=$2
  grep -F -- "$expected" "$file" >/dev/null || {
    printf 'Expected %s to contain: %s\n' "$file" "$expected" >&2
    cat "$file" >&2
    exit 1
  }
}

assert_not_contains() {
  file=$1
  unexpected=$2
  if grep -F -- "$unexpected" "$file" >/dev/null; then
    printf 'Expected %s not to contain: %s\n' "$file" "$unexpected" >&2
    cat "$file" >&2
    exit 1
  fi
}

expect_failure() {
  if "$@" >"$OUTPUT" 2>&1; then
    printf 'Expected command to fail: %s\n' "$*" >&2
    exit 1
  fi
}

# Parse/help and validation branches run under dash.
dash -n "$INSTALLER"
dash "$INSTALLER" --help >"$OUTPUT"
assert_contains "$OUTPUT" '--package-manager MANAGER'
expect_failure dash "$INSTALLER" --version 1.2.3/../../etc --dry-run
expect_failure dash "$INSTALLER" --package-manager apk --version 1.2.3 --dry-run
expect_failure dash "$INSTALLER" --prefix /usr --version 1.2.3 --dry-run
expect_failure dash "$INSTALLER" --from-source --prefix relative --dry-run
expect_failure dash "$INSTALLER" --from-source --prefix /tmp/../etc --dry-run
expect_failure dash "$INSTALLER" --from-source --version 1.2.3 --dry-run

# Refuse a genuinely privileged invocation, but do not mistake an inherited
# SUDO_USER variable for the process's effective identity.
reset_logs
SUDO_USER=root dash "$INSTALLER" \
  --version 1.2.3 --package-manager apt --dry-run --no-start >"$OUTPUT"
assert_contains "$OUTPUT" 'Dry run complete; no package was installed.'
MOCK_UID=0
export MOCK_UID
expect_failure dash "$INSTALLER" --version 1.2.3 --dry-run
assert_contains "$OUTPUT" 'run this installer as a regular user'
MOCK_UID=1000
export MOCK_UID

reset_logs
MOCK_ARCH=riscv64
export MOCK_ARCH
expect_failure dash "$INSTALLER" --version 1.2.3 --package-manager apt --dry-run
assert_contains "$OUTPUT" 'unsupported Linux architecture: riscv64'
[ ! -s "$MOCK_CURL_LOG" ]

reset_logs
MOCK_ARCH=x86_64
MOCK_LATEST_TAG=../../malicious
export MOCK_ARCH MOCK_LATEST_TAG
expect_failure dash "$INSTALLER" --package-manager apt --dry-run
assert_contains "$OUTPUT" 'GitHub returned an invalid release tag'
assert_not_contains "$MOCK_CURL_LOG" '/download/'
MOCK_LATEST_TAG=v9.8.7
export MOCK_LATEST_TAG

# Latest-version resolution, architecture detection, apt detection, checksum
# verification, privilege boundary and best-effort user service startup.
reset_logs
MOCK_ARCH=x86_64
export MOCK_ARCH
dash "$INSTALLER" >"$OUTPUT"
assert_contains "$MOCK_CURL_LOG" '--proto =https --proto-redir =https --tlsv1.2 --fail --location'
assert_contains "$MOCK_CURL_LOG" '/releases/latest'
assert_contains "$MOCK_CURL_LOG" '/download/v9.8.7/openlogi-v9.8.7-linux-amd64.deb'
assert_contains "$MOCK_CURL_LOG" '/download/v9.8.7/SHA256SUMS'
assert_contains "$MOCK_LOG" 'sudo apt-get install -y '
assert_contains "$MOCK_LOG" 'systemctl --user daemon-reload'
assert_contains "$MOCK_LOG" 'systemctl --user enable --now openlogi-agent.service'
assert_contains "$OUTPUT" 'Verified openlogi-v9.8.7-linux-amd64.deb before privilege escalation.'
[ "$(grep -c '^sudo ' "$MOCK_LOG")" -eq 1 ]
installed_temp=$(cat "$MOCK_PACKAGE_PATH")
[ ! -d "${installed_temp%/*}" ]

# Every supported override maps to the exact native package and command.
for specification in \
  apt:deb:apt-get \
  dnf:rpm:dnf \
  yum:rpm:yum \
  zypper:rpm:zypper \
  rpm:rpm:rpm \
  pacman:pkg.tar.zst:pacman; do
  manager=${specification%%:*}
  remainder=${specification#*:}
  extension=${remainder%%:*}
  package_command=${remainder##*:}
  reset_logs
  MOCK_ARCH=aarch64
  export MOCK_ARCH
  dash "$INSTALLER" --version v1.2.3 --package-manager "$manager" --no-start >"$OUTPUT"
  assert_contains "$MOCK_CURL_LOG" "/download/v1.2.3/openlogi-v1.2.3-linux-arm64.${extension}"
  assert_contains "$MOCK_LOG" "sudo ${package_command} "
  assert_not_contains "$MOCK_LOG" 'systemctl '
  [ "$(grep -c '^sudo ' "$MOCK_LOG")" -eq 1 ]
done

# Dry-run still proves the download and checksum but crosses no privilege or
# service boundary.
reset_logs
dash "$INSTALLER" --version 1.2.3 --package-manager rpm --dry-run >"$OUTPUT"
assert_contains "$OUTPUT" 'Would run: sudo rpm -Uvh '
assert_contains "$OUTPUT" 'Would run: systemctl --user enable --now openlogi-agent.service'
[ ! -s "$MOCK_LOG" ]

# A checksum mismatch must fail before sudo or the package manager is reached.
reset_logs
MOCK_BAD_SUM=1
export MOCK_BAD_SUM
expect_failure dash "$INSTALLER" --version 1.2.3 --package-manager apt
unset MOCK_BAD_SUM
assert_contains "$OUTPUT" 'checksum verification failed'
[ ! -s "$MOCK_LOG" ]

# Source mode retains all four local binaries and shared Linux resources while
# rewriting the packaged unit to the requested prefix.
reset_logs
SOURCE_REPO=${WORK_DIR}/source-repo
mkdir -p \
  "${SOURCE_REPO}/packaging/linux/udev" \
  "${SOURCE_REPO}/packaging/linux/systemd" \
  "${SOURCE_REPO}/packaging/linux/desktop" \
  "${SOURCE_REPO}/design/icon" \
  "${SOURCE_REPO}/target/release"
cp "$INSTALLER" "${SOURCE_REPO}/packaging/linux/install.sh"
cp "${REPO_ROOT}/packaging/linux/udev/70-openlogi.rules" \
  "${SOURCE_REPO}/packaging/linux/udev/70-openlogi.rules"
cp "${REPO_ROOT}/packaging/linux/systemd/openlogi-agent.service" \
  "${SOURCE_REPO}/packaging/linux/systemd/openlogi-agent.service"
cp "${REPO_ROOT}/packaging/linux/desktop/openlogi.desktop" \
  "${SOURCE_REPO}/packaging/linux/desktop/openlogi.desktop"
: >"${SOURCE_REPO}/design/icon/openlogi.png"
for binary in openlogi openlogi-desktop openlogi-overlay openlogi-agent; do
  printf '#!/bin/sh\n' >"${SOURCE_REPO}/target/release/${binary}"
  chmod +x "${SOURCE_REPO}/target/release/${binary}"
done

SOURCE_PREFIX=${WORK_DIR}/prefix
dash "${SOURCE_REPO}/packaging/linux/install.sh" \
  --from-source --prefix "$SOURCE_PREFIX" >"$OUTPUT"
for binary in openlogi openlogi-desktop openlogi-overlay openlogi-agent; do
  assert_contains "$MOCK_LOG" "/target/release/${binary} ${SOURCE_PREFIX}/bin/${binary}"
done
assert_contains "$MOCK_LOG" '/etc/udev/rules.d/70-openlogi.rules'
assert_contains "$MOCK_LOG" '/usr/lib/systemd/user/openlogi-agent.service'
assert_contains "$MOCK_LOG" '/usr/share/applications/openlogi.desktop'
assert_contains "$MOCK_LOG" '/usr/share/icons/hicolor/1024x1024/apps/openlogi.png'
assert_contains "$MOCK_SERVICE_CAPTURE" "ExecStart=${SOURCE_PREFIX}/bin/openlogi-agent"
assert_contains "$MOCK_LOG" 'systemctl --user enable --now openlogi-agent.service'

printf '%s\n' 'install.sh smoke tests passed'
