#!/usr/bin/env bash
# Start a headless X server, launch the real PyQt5 snuffler on synthetic data,
# and stream the GUI to the browser over noVNC (websockify on :8080).
set -euo pipefail

export DISPLAY=:99
GEOM="${GEOMETRY:-1600x900x24}"

rm -f /tmp/.X99-lock
Xvfb :99 -screen 0 "$GEOM" -ac +extension GLX +render -noreset &
for _ in $(seq 1 40); do
  xdpyinfo -display :99 >/dev/null 2>&1 && break
  sleep 0.25
done

openbox &
sleep 1

# Skip pyrocko's one-time "Thank you for installing" wizard (gated on the
# snuffler config's first_start flag) so the viewer is unobstructed.
python3 -c "from pyrocko import config; sc = config.config('snuffler'); sc.first_start = False; config.write_config(sc, 'snuffler')" || true

python3 /opt/snuffler/make_demo.py /tmp/demo.mseed

# snuffler defaults to software rendering on Linux (no OpenGL widget); force
# software GL anyway so it never needs a real GPU under Xvfb.
LIBGL_ALWAYS_SOFTWARE=1 QT_QPA_PLATFORM=xcb snuffler /tmp/demo.mseed &

# Maximize the Snuffler window to fill the canvas once it appears.
(
  for _ in $(seq 1 40); do
    wid=$(wmctrl -l 2>/dev/null | awk 'tolower($0) ~ /snuffler/ {print $1; exit}')
    if [ -n "$wid" ]; then
      wmctrl -i -r "$wid" -b add,maximized_vert,maximized_horz
      break
    fi
    sleep 0.5
  done
) &

x11vnc -display :99 -forever -shared -nopw -rfbport 5900 -quiet -bg

# noVNC: serve the web client and proxy WebSocket->VNC on one port.
exec websockify --web=/usr/share/novnc 8080 localhost:5900
