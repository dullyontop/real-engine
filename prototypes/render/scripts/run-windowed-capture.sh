#!/usr/bin/env bash
# Run the windowed spike on a private Xvfb display, capture the X root window
# while the app holds its last presented frame, then stop Xvfb.
# Usage: run-windowed-capture.sh <windowed-binary> <backend> <out.png> [frames]
set -u
BIN="$1"; BACKEND="$2"; OUT="$3"; FRAMES="${4:-10}"
# find a free display number
for n in $(seq 140 199); do
  if [ ! -e "/tmp/.X${n}-lock" ] && [ ! -e "/tmp/.X11-unix/X${n}" ]; then DISP=":$n"; break; fi
done
Xvfb "$DISP" -screen 0 1280x720x24 -nolisten tcp -noreset >/dev/null 2>&1 &
XPID=$!
for i in $(seq 1 50); do [ -e "/tmp/.X11-unix/X${DISP#:}" ] && break; sleep 0.1; done
echo "Xvfb pid=$XPID display=$DISP"
DISPLAY="$DISP" "$BIN" --backends "$BACKEND" --frames "$FRAMES" --hold-ms 2500 ${EXTRA_ARGS:-} &
APID=$!
# wait until the app reports it is holding (poll for up to 60 s), then capture
sleep 1.5
for i in $(seq 1 60); do
  if ! kill -0 "$APID" 2>/dev/null; then break; fi
  DISPLAY="$DISP" import -window root "$OUT" 2>/dev/null && break
  sleep 1
done
wait "$APID"; RC=$?
kill "$XPID" 2>/dev/null; wait "$XPID" 2>/dev/null
echo "app exit=$RC capture=$( [ -s "$OUT" ] && echo "$OUT" || echo none )"
exit $RC
