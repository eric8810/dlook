#!/usr/bin/env bash
# E9: 音频 E2E 探针可行性 —— null sink + monitor 录制,零外放证明样本到达设备
set -e
cd "$(dirname "$0")"
ffmpeg -hide_banner -loglevel error -y -f lavfi -i "sine=frequency=440:duration=3" -c:a pcm_s16le sine.wav
pactl load-module module-null-sink sink_name=dlook-e2e >/dev/null
pw-record --target dlook-e2e.monitor --channels 2 --rate 44100 /tmp/e2e-audio.wav &
REC_PID=$!
sleep 0.3
paplay --device=dlook-e2e sine.wav
sleep 0.5
kill $REC_PID 2>/dev/null || true
pactl unload-module module-null-sink >/dev/null 2>&1 || true
python3 - <<'PY'
import wave, struct, math
w = wave.open("/tmp/e2e-audio.wav")
frames = w.readframes(w.getnframes())
print(f"captured {w.getnframes()} frames @ {w.getframerate()}Hz ch={w.getnchannels()}")
all_samples = struct.unpack(f"<{len(frames)//2}h", frames)
samples = all_samples[::w.getnchannels()]
rms = math.sqrt(sum(v*v for v in samples) / len(samples))
print(f"RMS = {rms:.0f} (silence ~0)")
freq, rate = 440, 44100
k = 2*math.pi*freq/rate
s1 = s2 = 0.0
for v in samples:
    s0 = v + 2*math.cos(k)*s1 - s2
    s2, s1 = s1, s0
power = s1*s1 + s2*s2 - 2*math.cos(k)*s1*s2
print(f"440Hz Goertzel power = {power:.3e}")
print("VERDICT:", "REAL AUDIO SAMPLES REACHED DEVICE" if rms > 500 and power > 1e9 else "NOT CAPTURED")
PY
