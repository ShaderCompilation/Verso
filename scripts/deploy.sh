#!/usr/bin/env bash
# Builds Verso for the Kindle, copies it over SSH, starts it through the
# launcher (just like KUAL does), and streams its log. Ctrl-C stops Verso and
# returns the Kindle to its home screen.
#
#   scripts/deploy.sh                  build, copy, run
#   scripts/deploy.sh --no-run         build and copy only
#   RUST_LOG=debug scripts/deploy.sh   more detailed log
#
# The SSH host defaults to `kindle` (see docs/setup.md); override with VERSO_KINDLE_HOST.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HOST="${VERSO_KINDLE_HOST:-kindle}"
APP=/mnt/us/verso
EXT=/mnt/us/extensions/verso
PIDFILE=/tmp/verso-launcher.pid

RUN=yes
case "${1:-}" in
    --no-run) RUN=no ;;
    "") ;;
    *) echo "usage: $0 [--no-run]" >&2; exit 2 ;;
esac

cd "$ROOT"
echo "==> building"
cargo kindle-build
scripts/build-ssh.sh
scripts/build-wordforms.sh
cargo build --release --quiet -p verso-update --bin verso-release
# The same component folders a release is made from (docs/updates.md), and
# the installed.json that describes them, marked as a development install.
scripts/stage.sh
STAGE=target/package/stage
VERSION="$(cargo pkgid -p verso | sed 's/.*[#@]//')"
INSTALLED=target/package/installed-dev.json
HASHES="$(target/release/verso-release installed --components dist/components.json --stage "$STAGE" \
    --version "$VERSION" --dev --out "$INSTALLED")"
hash_of() { awk -v id="$1" '$1 == id { print $2 }' <<<"$HASHES"; }

echo "==> stopping Verso if it's running"
ssh "$HOST" sh -s <<'EOF'
killall -TERM verso 2>/dev/null || true
i=0
while [ "$i" -lt 100 ]; do
    if ! pidof verso >/dev/null 2>&1 &&
        { [ ! -s /tmp/verso-launcher.pid ] || ! kill -0 "$(cat /tmp/verso-launcher.pid)" 2>/dev/null; }; then
        exit 0
    fi
    usleep 100000
    i=$((i + 1))
done
echo "Verso did not stop; deployment cancelled before copying files" >&2
exit 1
EOF

echo "==> copying to $HOST:$APP and $EXT"
# Each component's hash on the device, from its installed.json (written last
# by a deploy, an update or the release zip), read before it is removed. An
# update replaces components too, so this, not a marker of our own, says
# whether the large data components need copying.
DEVICE_HASHES="$(ssh "$HOST" "cat $APP/installed.json 2>/dev/null" | python3 -I -c '
import json, sys
try:
    for c in json.load(sys.stdin)["components"]:
        print(c["id"], c["hash"])
except Exception:
    pass
' || true)"
device_hash_of() { awk -v id="$1" '$1 == id { print $2 }' <<<"$DEVICE_HASHES"; }
# An update staged or downloaded earlier would replace this build on the next
# start, and an earlier update's previous files, trial or journal would "go
# back" over it (docs/updates.md). A development install starts clean.
U=$APP/update
ssh "$HOST" "mkdir -p $APP $EXT && rm -f $APP/installed.json $APP/data/cre/.version $APP/wordforms/.version &&
    rm -rf $U/staged $U/download $U/previous $U/trial $U/rollback.json $U/rollback.json.new $U/journal-*"
tar -C "$STAGE/kual" --owner=0 --group=0 -cf - . | ssh "$HOST" "tar -xf - -C $EXT"
tar -C "$STAGE/launcher" --owner=0 --group=0 -cf - . | ssh "$HOST" "tar -xf - -C $APP"
# crengine's style sheets and hyphenation patterns, only when they changed.
if [ "$(device_hash_of crengine-data)" != "$(hash_of crengine-data)" ]; then
    echo "==> copying crengine data"
    ssh "$HOST" "rm -rf $APP/data/cre && mkdir -p $APP/data/cre"
    tar -C "$STAGE/crengine-data" --owner=0 --group=0 -cf - . | ssh "$HOST" "tar -xf - -C $APP/data/cre"
fi
# Book fonts and their licence, separate from crengine data.
ssh "$HOST" "mkdir -p $APP/fonts"
tar -C "$STAGE/fonts" --owner=0 --group=0 -cf - . | ssh "$HOST" "tar -xf - -C $APP/fonts"
# The Russian word-form pack with its attribution and licence, only when one
# of them changed. Unpacked next to the folder and moved in, so a broken
# transfer never leaves half a pack; other packs in the folder are kept.
if [ "$(device_hash_of wordforms-ru)" != "$(hash_of wordforms-ru)" ]; then
    echo "==> copying word forms"
    tar -C "$STAGE/wordforms-ru" --owner=0 --group=0 -cf - . |
        ssh "$HOST" "rm -rf $APP/wordforms.new && mkdir -p $APP/wordforms.new $APP/wordforms &&
            tar -xf - -C $APP/wordforms.new && for f in $APP/wordforms.new/*; do mv -f \$f $APP/wordforms/; done &&
            rmdir $APP/wordforms.new"
fi
# The SSH server (Settings → Developer), its mDNS responder and the keys
# folder. Unpacked next to bin/ and moved in, because a running server keeps
# its binary open. (Verso restarts a responder left on the old binary.)
tar -C "$STAGE/tools" --owner=0 --group=0 -cf - . | ssh "$HOST" "rm -rf $APP/bin.new && mkdir -p $APP/bin.new $APP/bin $APP/ssh &&
    tar -xf - -C $APP/bin.new && for f in $APP/bin.new/*; do mv -f \$f $APP/bin/; done && rmdir $APP/bin.new"
# Copy under a temporary name first, so a broken transfer never leaves half a binary.
ssh "$HOST" "cat > $APP/verso.new && chmod 755 $APP/verso.new && mv -f $APP/verso.new $APP/verso" <"$STAGE/app/verso"
# Removed before copying and written last, so it never describes files that
# weren't copied.
ssh "$HOST" "cat > $APP/installed.json.new && mv -f $APP/installed.json.new $APP/installed.json" <"$INSTALLED"

if [ "$RUN" = no ]; then
    echo "==> copied; start it from KUAL → Verso → Start Verso"
    exit 0
fi

stop() {
    echo
    echo "==> stopping Verso"
    ssh "$HOST" 'killall -TERM verso 2>/dev/null' || true
}
trap stop INT

echo "==> running (Ctrl-C to stop)"
# Start the launcher detached (like KUAL), then follow the log until it's done.
ssh "$HOST" sh -s <<EOF || true
LOG=$APP/verso.log
N=\$(cat \$LOG 2>/dev/null | wc -l)
RUST_LOG='${RUST_LOG:-info}' setsid sh $APP/verso.sh </dev/null >/dev/null 2>&1 &
sleep 1
# The launcher may have started a fresh log.
[ "\$(cat \$LOG 2>/dev/null | wc -l)" -lt "\$N" ] && N=0
tail -n +\$((N + 1)) -f \$LOG &
T=\$!
while [ -f $PIDFILE ]; do sleep 1; done
sleep 1
kill \$T
EOF
echo "==> Verso has exited"
