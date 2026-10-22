#!/usr/bin/env bash
# Inside the container: starts D-Bus, PipeWire, a headless sway, the portals and a fake camera,
# then runs the given command (the portal_capture example) against them.
set -euo pipefail

if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
  export XDG_RUNTIME_DIR=/tmp/runtime-tester
  mkdir -p -m 700 "${XDG_RUNTIME_DIR}"
  exec dbus-run-session -- "$0" "$@"
fi

export XDG_CURRENT_DESKTOP=sway
mkdir -p ~/.config/xdg-desktop-portal-wlr ~/.config/xdg-desktop-portal
cp /opt/portal-test/xdpw.conf ~/.config/xdg-desktop-portal-wlr/config
cp /opt/portal-test/portals.conf ~/.config/xdg-desktop-portal/sway-portals.conf

pipewire >/tmp/pipewire.log 2>&1 &
wireplumber >/tmp/wireplumber.log 2>&1 &
WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
  sway -c /opt/portal-test/sway.conf >/tmp/sway.log 2>&1 &
for _ in $(seq 50); do [ -S "${XDG_RUNTIME_DIR}/wayland-1" ] && break; sleep 0.1; done
export WAYLAND_DISPLAY=wayland-1
dbus-update-activation-environment WAYLAND_DISPLAY XDG_CURRENT_DESKTOP XDG_RUNTIME_DIR
# Something moving on the screen (a static headless output produces no new frames), and a
# PipeWire video source marked as a camera for the camera portal.
gst-launch-1.0 -q videotestsrc is-live=true pattern=smpte ! video/x-raw,width=800,height=600 \
  ! waylandsink fullscreen=true >/tmp/screen-content.log 2>&1 &
gst-launch-1.0 -q videotestsrc is-live=true pattern=ball ! video/x-raw,width=640,height=480,framerate=30/1 \
  ! pipewiresink mode=provide \
    stream-properties='properties,media.class=(string)"Video/Source",media.role=(string)Camera,node.description=(string)TestCamera,node.driver=(boolean)true,priority.driver=(int)20000' \
  >/tmp/camera.log 2>&1 &
sleep 2
# No user to answer the camera access dialog: grant it up front for unsandboxed apps (empty app
# id), as a user would once.
gdbus call --session --dest org.freedesktop.impl.portal.PermissionStore \
  --object-path /org/freedesktop/impl/portal/PermissionStore \
  --method org.freedesktop.impl.portal.PermissionStore.SetPermission \
  devices true camera "" "['yes']" >/dev/null

status=0
timeout 60 "$@" || status=$?
if [ "${status}" -ne 0 ]; then
  for log in /tmp/screen-content.log /tmp/sway.log /tmp/pipewire.log /tmp/wireplumber.log /tmp/camera.log; do
    echo "== ${log}"; tail -20 "${log}"
  done
fi
exit "${status}"
