#!/usr/bin/env python3
"""Writes the valley's sounds: synthesised 16-bit mono WAV files.

- `assets/sfx/step_a.wav`, `assets/sfx/step_b.wav`: footsteps on grass, a
  soft thud under a short rustle (two takes, played in turn).
- `assets/sfx/land.wav`: landing after a jump or a fall, a heavier thud.
- `assets/sfx/door.wav`: the car door, a low clunk with a click.
- `assets/sfx/engine_idle.wav`, `assets/sfx/engine_mid.wav`,
  `assets/sfx/engine_high.wav`: the car's engine at about 1000, 2800 and
  5000 rpm, 1 s loops of a four-cylinder's exhaust pulses over its firing
  tone. The car script crossfades them by the engine's rpm.
- `assets/ambience/wind.wav`: wind, a 6 s loop of low-passed noise that
  swells and settles (at 11,025 Hz: it has nothing above 1 kHz).

Every sound is made here from sines and seeded noise; there are no recorded
samples. Loops hold a whole number of cycles of every tone and of their
modulation, so they repeat without a click. Standard library only. Run from
the project directory:

    python3 tools/make_sounds.py

The output is byte-for-byte the same on every run (on one platform: Python's
`math` uses the platform's libm), so the committed files can be checked with
`git diff --exit-code` after running it.
"""

import math
import random
import struct
import sys
from pathlib import Path

RATE = 22050


def wav(samples, rate=RATE):
    """16-bit PCM mono from floats in -1..1 (clamped)."""
    pcm = bytearray()
    for s in samples:
        v = max(-1.0, min(1.0, s))
        pcm += struct.pack("<h", int(round(v * 32767)))
    head = b"RIFF" + struct.pack("<I", 36 + len(pcm)) + b"WAVE"
    fmt = b"fmt " + struct.pack("<IHHIIHH", 16, 1, 1, rate, rate * 2, 2, 16)
    return head + fmt + b"data" + struct.pack("<I", len(pcm)) + bytes(pcm)


def normalise(samples, peak):
    top = max(abs(s) for s in samples) or 1.0
    return [s * peak / top for s in samples]


def low_pass(samples, cutoff, passes=1, loop=False, rate=RATE):
    """A one-pole low-pass, `passes` times. With `loop`, the filter runs over
    the buffer twice and keeps the second pass, so the end flows into the
    start."""
    a = 1.0 - math.exp(-2.0 * math.pi * cutoff / rate)
    out = list(samples)
    for _ in range(passes):
        y = 0.0
        if loop:
            for s in out:
                y += a * (s - y)
        filtered = []
        for s in out:
            y += a * (s - y)
            filtered.append(y)
        out = filtered
    return out


def high_pass(samples, cutoff):
    lows = low_pass(samples, cutoff)
    return [s - l for s, l in zip(samples, lows)]


def thump(seconds, low_hz, decay, rustle, seed):
    """A thud of `low_hz` (plus up to 20 Hz, from `seed`) dying in `decay`
    seconds, under a burst of high-passed noise `rustle` times as loud."""
    rng = random.Random(seed)
    n = int(seconds * RATE)
    f = low_hz + 20.0 * rng.random()
    noise = high_pass([rng.uniform(-1.0, 1.0) for _ in range(n)], 900.0)
    noise = low_pass(noise, 5000.0)
    out = []
    for i in range(n):
        t = i / RATE
        thud = math.sin(2.0 * math.pi * f * t) * math.exp(-t / decay)
        grass = noise[i] * math.exp(-t / 0.035) * min(1.0, t / 0.004)
        out.append(0.8 * thud + rustle * grass)
    return out


def step(seed):
    """0.16 s: a 80-100 Hz thud that dies in 40 ms under a short rustle."""
    return normalise(thump(0.16, 80.0, 0.04, 1.4, seed), 0.7)


def land():
    """0.3 s: a deeper, longer thud than a step, with a louder rustle."""
    return normalise(thump(0.3, 50.0, 0.08, 1.1, 5), 0.9)


def door():
    """0.35 s: a 62 Hz clunk with a body resonance at 180 Hz and a 5 ms click."""
    rng = random.Random(7)
    n = int(0.35 * RATE)
    out = []
    for i in range(n):
        t = i / RATE
        clunk = math.sin(2.0 * math.pi * 62.0 * t) * math.exp(-t / 0.09)
        body = 0.5 * math.sin(2.0 * math.pi * 180.0 * t) * math.exp(-t / 0.05)
        click = rng.uniform(-1.0, 1.0) * math.exp(-t / 0.005)
        out.append(clunk + body + 0.6 * click)
    return normalise(low_pass(out, 3000.0), 0.8)


def engine(firing, seed):
    """1 s loop of an engine firing `firing` times a second (an even whole
    number, so the half-rate crank tone fits the loop too): a burst of
    low-passed noise at each firing, the exhaust, over the firing tone and
    its harmonics."""
    rng = random.Random(seed)
    n = RATE
    grain = low_pass([rng.uniform(-1.0, 1.0) for _ in range(n)], 6.0 * firing, loop=True)
    grain = normalise(grain, 1.0)
    harmonics = [(0.5, 0.25), (1, 1.0), (2, 0.5), (3, 0.3), (4, 0.15)]
    decay = 0.35 / firing
    out = []
    for i in range(n):
        t = i / n
        phase = (firing * t) % 1.0
        burst = math.exp(-phase / (decay * firing))
        tone = sum(a * math.sin(2.0 * math.pi * firing * k * t) for k, a in harmonics)
        out.append(0.5 * tone * (0.6 + 0.4 * burst) + 0.9 * grain[i] * burst)
    return normalise(out, 0.6)


WIND_RATE = 11025


def wind():
    """6 s loop: noise low-passed twice around 400 Hz, swelling twice a loop
    with a slower third gust, every swell a whole number of cycles a loop."""
    rng = random.Random(11)
    n = 6 * WIND_RATE
    noise = [rng.uniform(-1.0, 1.0) for _ in range(n)]
    air = low_pass(noise, 400.0, passes=2, loop=True, rate=WIND_RATE)
    out = []
    for i in range(n):
        t = i / n
        swell = 0.6 + 0.25 * math.sin(2.0 * math.pi * 2 * t) + 0.15 * math.sin(2.0 * math.pi * 3 * t + 1.0)
        out.append(air[i] * swell)
    return normalise(out, 0.5)


SOUNDS = {
    "assets/sfx/step_a.wav": (lambda: step(1), RATE),
    "assets/sfx/step_b.wav": (lambda: step(2), RATE),
    "assets/sfx/land.wav": (land, RATE),
    "assets/sfx/door.wav": (door, RATE),
    "assets/sfx/engine_idle.wav": (lambda: engine(34, 3), RATE),
    "assets/sfx/engine_mid.wav": (lambda: engine(94, 4), RATE),
    "assets/sfx/engine_high.wav": (lambda: engine(166, 6), RATE),
    "assets/ambience/wind.wav": (wind, WIND_RATE),
}


if __name__ == "__main__":
    root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(".")
    for name, (make, rate) in SOUNDS.items():
        target = root / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(wav(make(), rate))
        print(f"wrote {target} ({target.stat().st_size} bytes)")
