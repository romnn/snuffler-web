# The ORIGINAL PyQt5 snuffler, in the browser — full feature completeness.
#
# Qt-for-Python cannot be compiled to WebAssembly (see
# ../plan/qt-frontend-feasibility.md). So instead of porting, we run the real,
# unmodified desktop snuffler under a headless X server and stream its GUI to the
# browser over noVNC. This is the actual pyrocko snuffler — every feature works.
#
# This is fully independent of the Rust/egui frontend (own image, own port).
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates \
      python3 python3-pip python3-setuptools \
      python3-numpy python3-scipy python3-matplotlib python3-yaml python3-requests \
      python3-pyqt5 python3-pyqt5.qtsvg python3-pyqt5.qtopengl \
      xvfb x11vnc novnc websockify openbox wmctrl \
      x11-xserver-utils x11-utils dbus-x11 \
      libgl1-mesa-dri libglu1-mesa libgl1 libegl1 \
      fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/*

# pyrocko from PyPI (manylinux wheel — no compilation). --no-deps: numpy/scipy/
# matplotlib/pyyaml/requests/PyQt5 are all provided by apt above.
RUN pip install --break-system-packages --no-deps pyrocko

COPY make_demo.py entrypoint.sh /opt/snuffler/
RUN chmod +x /opt/snuffler/entrypoint.sh

EXPOSE 8080
ENTRYPOINT ["/opt/snuffler/entrypoint.sh"]
