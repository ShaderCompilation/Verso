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
BIN="$ROOT/target/armv7-unknown-linux-gnueabihf/release/verso"
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
ssh "$HOST" "mkdir -p $APP $EXT"
tar -C dist/kindle/extensions/verso --owner=0 --group=0 -cf - . | ssh "$HOST" "tar -xf - -C $EXT"
tar -C dist/kindle/verso --owner=0 --group=0 -cf - . | ssh "$HOST" "tar -xf - -C $APP"
# crengine's style sheets and hyphenation patterns, only when crengine changed.
CRE_DATA=third_party/crengine/cr3gui/data
CRE_VERSION="$(git -C third_party/crengine rev-parse HEAD)"
if [ "$(ssh "$HOST" "cat $APP/data/cre/.version 2>/dev/null")" != "$CRE_VERSION" ]; then
    echo "==> copying crengine data"
    ssh "$HOST" "rm -rf $APP/data/cre && mkdir -p $APP/data/cre"
    tar -C "$CRE_DATA" --owner=0 --group=0 -cf - epub.css html5.css hyph | ssh "$HOST" "tar -xf - -C $APP/data/cre"
    echo "$CRE_VERSION" | ssh "$HOST" "cat > $APP/data/cre/.version"
fi
# Book fonts and their licence, separate from crengine data.
ssh "$HOST" "mkdir -p $APP/fonts"
tar -C assets/fonts --owner=0 --group=0 -cf - Literata-Regular.ttf Literata-Italic.ttf Literata-SemiBold.ttf Literata-SemiBoldItalic.ttf Literata-LICENSE.txt | ssh "$HOST" "tar -xf - -C $APP/fonts"
# The SSH server (Settings → Developer), its mDNS responder and the keys
# folder. Unpacked next to bin/ and moved in, because a running server keeps
# its binary open. (Verso restarts a responder left on the old binary.)
tar --owner=0 --group=0 -cf - -C target/kindle-ssh/bin . -C "$(dirname "$BIN")" verso-mdns | ssh "$HOST" "rm -rf $APP/bin.new && mkdir -p $APP/bin.new $APP/bin $APP/ssh &&
    tar -xf - -C $APP/bin.new && for f in $APP/bin.new/*; do mv -f \$f $APP/bin/; done && rmdir $APP/bin.new"
# Copy under a temporary name first, so a broken transfer never leaves half a binary.
ssh "$HOST" "cat > $APP/verso.new && chmod 755 $APP/verso.new && mv -f $APP/verso.new $APP/verso" <"$BIN"

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
