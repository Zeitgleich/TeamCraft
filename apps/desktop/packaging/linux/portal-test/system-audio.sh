#!/usr/bin/env bash
# Inside the container (after session.sh set up PipeWire): a null sink to play into, another
# program playing a 1 kHz tone, then the system_audio_capture example, which plays its own
# 300 Hz tone and checks that only the other program's sound is captured.
set -euo pipefail

pw-cli create-node adapter \
  '{ factory.name=support.null-audio-sink media.class=Audio/Sink node.name=test-sink object.linger=true audio.position=[FL FR] }' \
  >/dev/null
gst-launch-1.0 -q audiotestsrc num-buffers=1500 freq=1000 volume=0.3 \
  ! audio/x-raw,rate=48000,channels=2 ! wavenc ! filesink location=/tmp/other.wav
pw-play /tmp/other.wav >/tmp/other-app.log 2>&1 &
sleep 1
/opt/system_audio_capture
