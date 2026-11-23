#!/usr/bin/env bash
# Builds Verso's SSH server for the Kindle: upstream Dropbear and OpenSSH's
# sftp-server (for scp and sftp), as small static armv7 binaries (musl libc)
# with zig cc. scripts/deploy.sh runs this and installs the result in
# /mnt/us/verso/bin.
#
# Output: target/kindle-ssh/bin/ (the two programs and their licences).
# Does nothing when the submodules, options and this script are unchanged.
# Builds from copies of the sources, so third_party/ stays untouched.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
OUT=target/kindle-ssh
BIN="$OUT/bin"
OPTIONS=scripts/ssh/localoptions.h
ZIG="${ZIG:-zig}"

STAMP="$({
    git -C third_party/dropbear rev-parse HEAD
    git -C third_party/openssh-portable rev-parse HEAD
    cat "$OPTIONS" "$0"
    "$ZIG" version
} | sha256sum | cut -d' ' -f1)"
if [ -x "$BIN/dropbear" ] && [ -x "$BIN/sftp-server" ] && [ "$(cat "$OUT/.stamp" 2>/dev/null)" = "$STAMP" ]; then
    echo "==> SSH server is up to date"
    exit 0
fi

echo "==> building the SSH server (dropbear, sftp-server)"
BUILD="$ROOT/$OUT/build"
rm -rf "$BUILD" "$BIN" "$OUT/.stamp"
mkdir -p "$BUILD/dropbear" "$BUILD/openssh" "$BIN"

# Same CPU as the Rust armv7 target. musl instead of the Kindle's glibc, so
# the programs don't depend on the firmware's libraries.
cat >"$BUILD/cc" <<EOF
#!/bin/sh
exec "$ZIG" cc -target arm-linux-musleabihf -mcpu=generic+v7a+vfp3-d32+thumb2-neon "\$@"
EOF
chmod +x "$BUILD/cc"
# Size tips from Dropbear's SMALL.md: -Os, drop unused code at link time, strip.
export CC="$BUILD/cc" AR="$ZIG ar" RANLIB="$ZIG ranlib"
export CFLAGS="-Os -ffunction-sections -fdata-sections"
export LDFLAGS="-static -s -Wl,--gc-sections"
# No login records: they'd write to /var/log, outside Verso's folders.
NO_RECORDS="--disable-lastlog --disable-utmp --disable-utmpx --disable-wtmp --disable-wtmpx"
JOBS="$(nproc)"

# Exactly the committed sources (no .git file, which only works in place).
git -C third_party/dropbear archive HEAD | tar -x -C "$BUILD/dropbear"
git -C third_party/openssh-portable archive HEAD | tar -x -C "$BUILD/openssh"

(
    cd "$BUILD/dropbear"
    cp "$ROOT/$OPTIONS" localoptions.h
    # shellcheck disable=SC2086
    ./configure --host=arm-linux-musleabihf --enable-static --disable-zlib $NO_RECORDS \
        --disable-pututline --disable-pututxline LTM_CFLAGS=-Os >configure.log 2>&1 ||
        { tail -20 configure.log; exit 1; }
    # PROGRAMS=dropbear leaves out client code (SMALL.md).
    make -j"$JOBS" PROGRAMS=dropbear >make.log 2>&1 || { tail -30 make.log; exit 1; }
)
(
    cd "$BUILD/openssh"
    touch configure # configure refuses to run if configure.ac looks newer
    # shellcheck disable=SC2086
    ./configure --host=arm-linux-musleabihf --without-openssl --without-zlib --without-pam \
        --without-selinux --without-stackprotect --without-hardening $NO_RECORDS >configure.log 2>&1 ||
        { tail -20 configure.log; exit 1; }
    make -j"$JOBS" sftp-server >make.log 2>&1 || { tail -30 make.log; exit 1; }
)

cp "$BUILD/dropbear/dropbear" "$BUILD/openssh/sftp-server" "$BIN/"
cp third_party/dropbear/LICENSE "$BIN/LICENSE-dropbear"
cp third_party/openssh-portable/LICENCE "$BIN/LICENCE-openssh"
echo "$STAMP" >"$OUT/.stamp"
ls -l "$BIN/dropbear" "$BIN/sftp-server" | awk '{print "    " $5 " bytes  " $NF}'
