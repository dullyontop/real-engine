#!/usr/bin/env python3
"""Writes Town Courier's sounds: synthesised 16-bit mono WAV files.

- `assets/sfx/engine_idle.wav`, `assets/sfx/engine_mid.wav`,
  `assets/sfx/engine_high.wav`: the truck's diesel at 1020, 2580 and 4380 rpm,
  1 s loops of a four-cylinder firing 34, 86 and 146 times a second: a
  clatter and an exhaust pulse at each firing over the firing tone and a
  strong half-order rumble. truck.luau crossfades them by the engine's rpm
  and pitches them to it.
- `assets/sfx/horn.wav`: the truck's two-tone air horn.
- `assets/sfx/crash.wav`: a knock into something hard: a thud, a burst of
  noise and a few ringing metal partials.
- `assets/sfx/thud.wav`: a parcel landing on the road.
- `assets/sfx/load.wav`: parcels stacked on the bed: two cardboard thumps and
  a rising blip.
- `assets/sfx/deliver.wav`: a delivery, a bright three-note bell arpeggio.
- `assets/sfx/lost.wav`: a parcel lost, two falling notes.
- `assets/sfx/whistle.wav`: the shift starts, two blasts of a pea whistle.
- `assets/sfx/bell.wav`: the shift is over, two strikes of a bell.
- `assets/music/theme.wav`: the music, an 8 s loop at 120 bpm (four bars of
  C, A minor, F and G): bass, plucked chords, a lead tune and drums.
- `assets/ambience/town.wav`: the town, an 8 s loop of distant traffic, a
  breeze and birds (at 11,025 Hz, plenty for it).

Every sound is made here from sines and seeded noise; there are no recorded
samples. Loops hold a whole number of cycles of every tone and of their
modulation, and a note or chirp that runs past a loop's end continues at
its start, so they repeat without a click. Standard library only. Run from
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
TAU = 2.0 * math.pi


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
    a = 1.0 - math.exp(-TAU * cutoff / rate)
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


def high_pass(samples, cutoff, loop=False, rate=RATE):
    lows = low_pass(samples, cutoff, loop=loop, rate=rate)
    return [s - l for s, l in zip(samples, lows)]


def noise(n, seed):
    rng = random.Random(seed)
    return [rng.uniform(-1.0, 1.0) for _ in range(n)]


def add(out, at, samples, gain=1.0, wrap=False):
    """Mixes `samples` into `out` from sample `at`; with `wrap`, what runs
    past the end continues at the start (a loop)."""
    n = len(out)
    for i, s in enumerate(samples):
        j = at + i
        if j >= n:
            if not wrap:
                break
            j %= n
        out[j] += gain * s


def midi_hz(note):
    return 440.0 * 2.0 ** ((note - 69) / 12.0)


# ---- the truck -------------------------------------------------------------


def engine(firing, seed):
    """1 s loop of a diesel firing `firing` times a second (an even whole
    number, so the half-order tones fit the loop too): at each firing a
    clatter (a 1.5 ms tick of high-passed noise) and an exhaust pulse
    (low-passed noise dying over a third of the firing), over the firing
    tone, its harmonics and a strong half-order rumble."""
    n = RATE
    grain = normalise(low_pass(noise(n, seed), 5.0 * firing, loop=True), 1.0)
    tick = normalise(high_pass(noise(n, seed + 100), 1800.0, loop=True), 1.0)
    harmonics = [(0.5, 0.7), (1.0, 1.0), (1.5, 0.35), (2.0, 0.45), (3.0, 0.22), (4.0, 0.1)]
    rng = random.Random(seed + 200)
    # How hard each cylinder fires, the same every loop.
    strength = [0.75 + 0.25 * rng.random() for _ in range(4)]
    out = []
    for i in range(n):
        t = i / n
        cycle = firing * t
        k = int(cycle)
        phase = cycle - k
        hit = strength[k % 4]
        exhaust = math.exp(-phase / 0.33) * hit
        clatter = math.exp(-phase / (0.0015 * firing)) * hit
        tone = sum(a * math.sin(TAU * firing * h * t) for h, a in harmonics)
        out.append(0.45 * tone * (0.65 + 0.35 * exhaust) + 0.8 * grain[i] * exhaust + 0.25 * tick[i] * clatter)
    return normalise(out, 0.6)


def horn():
    """0.75 s: two sawtooth-like tones a major third apart (277 and 349 Hz),
    bending up into pitch over the first 60 ms, with an airy attack."""
    n = int(0.75 * RATE)
    breath = low_pass(high_pass(noise(n, 21), 600.0), 3000.0)
    phases = [0.0, 0.0]
    out = []
    for i in range(n):
        t = i / RATE
        bend = 0.9 + 0.1 * min(1.0, t / 0.06)
        env = min(1.0, t / 0.025) * min(1.0, (0.75 - t) / 0.09)
        s = 0.0
        for j, f in enumerate((277.2, 349.2)):
            phases[j] += TAU * f * bend / RATE
            s += sum(math.sin(k * phases[j]) / k for k in range(1, 11))
        out.append(env * (0.5 * s + 0.6 * breath[i] * math.exp(-t / 0.05)))
    return normalise(low_pass(out, 2600.0), 0.8)


def crash():
    """1 s: a 58 Hz thud, a burst of noise and four inharmonic metal
    partials ringing out."""
    n = RATE
    burst = low_pass(noise(n, 31), 4500.0)
    metal = [(431.0, 0.35, 0.5), (1127.0, 0.25, 0.35), (1793.0, 0.18, 0.25), (2511.0, 0.12, 0.18)]
    out = []
    for i in range(n):
        t = i / RATE
        thud = math.sin(TAU * 58.0 * t) * math.exp(-t / 0.12)
        hit = burst[i] * math.exp(-t / 0.07)
        ring = sum(a * math.sin(TAU * f * t) * math.exp(-t / d) for f, d, a in metal)
        out.append(thud + 0.9 * hit + 0.4 * ring)
    return normalise(out, 0.9)


def thump(seconds, low_hz, decay, rustle, seed):
    """A thud of `low_hz` dying in `decay` seconds, under a papery burst of
    band-passed noise `rustle` times as loud."""
    n = int(seconds * RATE)
    paper = low_pass(high_pass(noise(n, seed), 700.0), 3500.0)
    out = []
    for i in range(n):
        t = i / RATE
        thud = math.sin(TAU * low_hz * t) * math.exp(-t / decay)
        out.append(0.8 * thud + rustle * paper[i] * math.exp(-t / 0.03) * min(1.0, t / 0.003))
    return out


def thud():
    """0.35 s: a parcel landing, a cardboard thump."""
    return normalise(thump(0.35, 95.0, 0.05, 1.3, 41), 0.8)


def blip(seconds, f0, f1, harmonics=((1, 1.0), (2, 0.3))):
    """A soft note gliding from `f0` to `f1` Hz."""
    n = int(seconds * RATE)
    phase = 0.0
    out = []
    for i in range(n):
        t = i / RATE
        f = f0 + (f1 - f0) * min(1.0, t / (0.6 * seconds))
        phase += TAU * f / RATE
        env = min(1.0, t / 0.006) * math.exp(-t / (0.35 * seconds))
        out.append(env * sum(a * math.sin(k * phase) for k, a in harmonics))
    return out


def load():
    """0.5 s: two cardboard thumps 0.11 s apart, then a blip rising from
    660 to 990 Hz."""
    out = [0.0] * int(0.5 * RATE)
    add(out, 0, thump(0.2, 110.0, 0.04, 1.2, 51), 0.9)
    add(out, int(0.11 * RATE), thump(0.2, 90.0, 0.045, 1.2, 52), 0.8)
    add(out, int(0.24 * RATE), blip(0.25, 660.0, 990.0), 0.45)
    return normalise(out, 0.8)


def bell_note(seconds, f, partials, decay):
    """A struck bell: `partials` are (ratio, amplitude) pairs; higher
    partials die sooner."""
    n = int(seconds * RATE)
    out = []
    for i in range(n):
        t = i / RATE
        env = min(1.0, t / 0.002)
        s = sum(a * math.sin(TAU * f * r * t) * math.exp(-t * r / decay) for r, a in partials)
        out.append(env * s)
    return out


GLASS = ((1.0, 1.0), (2.0, 0.35), (3.01, 0.18), (4.2, 0.08))


def deliver():
    """0.9 s: C6, E6 and G6 struck 90 ms apart, a bright bell for each."""
    out = [0.0] * int(0.9 * RATE)
    for k, note in enumerate((84, 88, 91)):
        add(out, int(k * 0.09 * RATE), bell_note(0.9, midi_hz(note), GLASS, 0.35), 1.0)
    return normalise(out, 0.75)


def lost():
    """0.65 s: G4 then D4, a soft square tone with a slow vibrato."""
    out = [0.0] * int(0.65 * RATE)
    square = ((1, 1.0), (3, 0.3), (5, 0.12), (7, 0.05))
    for start, length, note in ((0.0, 0.2, 67), (0.2, 0.45, 62)):
        n = int(length * RATE)
        f = midi_hz(note)
        phase = 0.0
        tone = []
        for i in range(n):
            t = i / RATE
            phase += TAU * f * (1.0 + 0.012 * math.sin(TAU * 5.0 * t)) / RATE
            env = min(1.0, t / 0.01) * min(1.0, (length - t) / 0.05)
            tone.append(env * sum(a * math.sin(k * phase) for k, a in square))
        add(out, int(start * RATE), tone)
    return normalise(low_pass(out, 2400.0), 0.45)


def whistle():
    """0.85 s: two blasts (0.25 s and 0.45 s) of a 2.6 kHz pea whistle whose
    pea trills its pitch and loudness 32 times a second, with breath."""
    n = int(0.85 * RATE)
    breath = low_pass(high_pass(noise(n, 61), 1800.0), 5000.0)
    phase = 0.0
    out = []
    for i in range(n):
        t = i / RATE
        if t < 0.25:
            env = min(1.0, t / 0.01) * min(1.0, (0.25 - t) / 0.02)
        elif 0.33 <= t < 0.78:
            env = min(1.0, (t - 0.33) / 0.01) * min(1.0, (0.78 - t) / 0.04)
        else:
            env = 0.0
        trill = math.sin(TAU * 32.0 * t)
        phase += TAU * (2600.0 + 90.0 * trill) / RATE
        out.append(env * ((0.75 + 0.25 * trill) * math.sin(phase) + 0.25 * breath[i]))
    return normalise(out, 0.6)


CHURCH = ((0.5, 0.6), (1.0, 1.0), (1.19, 0.5), (1.5, 0.35), (2.0, 0.4), (2.51, 0.15), (2.99, 0.1))


def bell():
    """3 s: a bell struck at 330 Hz, then at 247 Hz 0.9 s later."""
    out = [0.0] * (3 * RATE)
    add(out, 0, bell_note(3.0, 330.0, CHURCH, 1.4), 1.0)
    add(out, int(0.9 * RATE), bell_note(2.1, 247.0, CHURCH, 1.4), 0.9)
    return normalise(out, 0.8)


# ---- the music ---------------------------------------------------------------

BEAT = 0.5  # seconds: 120 bpm
BARS = 4
LOOP = BARS * 4 * BEAT  # 8 s

# One chord a bar: its root (for the bass) and its notes (MIDI).
CHORDS = [
    (48, (60, 64, 67)),  # C
    (45, (57, 60, 64)),  # A minor
    (41, (57, 60, 65)),  # F
    (43, (55, 59, 62)),  # G
]

# The tune: (start beat, beats, MIDI note).
TUNE = [
    (0, 0.5, 76), (0.5, 0.5, 79), (1, 1, 81), (2, 0.5, 79), (2.5, 0.5, 76), (3, 1, 74),
    (4, 0.5, 72), (4.5, 0.5, 76), (5, 1, 81), (6, 0.5, 79), (6.5, 0.5, 76), (7, 1, 72),
    (8, 0.5, 77), (8.5, 0.5, 81), (9, 1, 84), (10, 0.5, 81), (10.5, 0.5, 77), (11, 1, 79),
    (12, 0.5, 79), (12.5, 0.5, 83), (13, 1, 86), (14, 0.5, 83), (14.5, 0.5, 79), (15, 1.5, 74),
]


def tone(seconds, f, harmonics, attack, release, vibrato=0.0):
    n = int(seconds * RATE)
    phase = 0.0
    out = []
    for i in range(n):
        t = i / RATE
        phase += TAU * f * (1.0 + vibrato * math.sin(TAU * 5.5 * t)) / RATE
        env = min(1.0, t / attack) * min(1.0, max(0.0, seconds - t) / release)
        out.append(env * sum(a * math.sin(k * phase) for k, a in harmonics))
    return out


def pluck(seconds, f, decay):
    n = int(seconds * RATE)
    out = []
    for i in range(n):
        t = i / RATE
        env = min(1.0, t / 0.003) * math.exp(-t / decay)
        out.append(env * (math.sin(TAU * f * t) + 0.4 * math.sin(TAU * 2 * f * t) + 0.15 * math.sin(TAU * 3 * f * t)))
    return out


def kick():
    n = int(0.18 * RATE)
    phase = 0.0
    out = []
    for i in range(n):
        t = i / RATE
        phase += TAU * (48.0 + 90.0 * math.exp(-t / 0.03)) / RATE
        out.append(math.sin(phase) * math.exp(-t / 0.07))
    return out


def snare(seed):
    n = int(0.16 * RATE)
    rattle = low_pass(high_pass(noise(n, seed), 1200.0), 6000.0)
    return [
        (0.5 * math.sin(TAU * 185.0 * (i / RATE)) * math.exp(-(i / RATE) / 0.03) + rattle[i] * math.exp(-(i / RATE) / 0.05))
        for i in range(n)
    ]


def hat(seed):
    n = int(0.04 * RATE)
    tick = high_pass(noise(n, seed), 5000.0)
    return [tick[i] * math.exp(-(i / RATE) / 0.012) for i in range(n)]


def music():
    """The 8 s loop: an oom-pah bass on each bar's root, the chord plucked
    on every off-beat, the tune on a soft square lead, a kick on beats 1 and
    3, a snare on 2 and 4 and a hi-hat on every eighth."""
    out = [0.0] * int(LOOP * RATE)
    at = lambda beat: int(round(beat * BEAT * RATE))
    triangle = ((1, 1.0), (3, -1.0 / 9), (5, 1.0 / 25))
    lead = ((1, 1.0), (3, 0.28), (5, 0.1), (7, 0.04))
    for bar, (root, notes) in enumerate(CHORDS):
        for beat in range(4):
            b = bar * 4 + beat
            bass = root if beat % 2 == 0 else root + 12
            add(out, at(b), tone(0.42, midi_hz(bass), triangle, 0.005, 0.08), 0.42, wrap=True)
            for k, note in enumerate(notes):
                # A strum: each string a few milliseconds after the last.
                add(out, at(b + 0.5) + k * 90, pluck(0.45, midi_hz(note), 0.16), 0.13, wrap=True)
            add(out, at(b), kick() if beat % 2 == 0 else snare(70 + b), 0.55 if beat % 2 == 0 else 0.3, wrap=True)
            for e in range(2):
                add(out, at(b + 0.5 * e), hat(90 + 2 * b + e), 0.07 if e else 0.1, wrap=True)
    for start, beats, note in TUNE:
        add(out, at(start), tone(beats * BEAT, midi_hz(note), lead, 0.012, 0.06, 0.004), 0.2, wrap=True)
    out = low_pass(out, 7000.0, loop=True)
    return normalise(out, 0.7)


# ---- the town ----------------------------------------------------------------

AMBIENCE_RATE = 11025


def ambience():
    """8 s loop: distant traffic (noise low-passed twice at 140 Hz, swelling
    twice a loop), a breeze (noise low-passed at 700 Hz, swelling three
    times) and birds: 7 calls of 2 to 4 quick chirps each, sweeping between
    2.6 and 4.4 kHz, placed at random."""
    rate = AMBIENCE_RATE
    n = 8 * rate
    traffic = normalise(low_pass(noise(n, 81), 140.0, passes=2, loop=True, rate=rate), 1.0)
    breeze = normalise(low_pass(noise(n, 82), 700.0, loop=True, rate=rate), 1.0)
    out = []
    for i in range(n):
        t = i / n
        swell = 0.75 + 0.25 * math.sin(TAU * 2 * t)
        gust = 0.6 + 0.4 * math.sin(TAU * 3 * t + 1.3)
        out.append(0.55 * traffic[i] * swell + 0.12 * breeze[i] * gust)
    rng = random.Random(83)
    for _ in range(7):
        start = rng.randrange(n)
        chirps = rng.randint(2, 4)
        top = rng.uniform(3400.0, 4400.0)
        low = rng.uniform(2600.0, 3200.0)
        length = rng.uniform(0.05, 0.09)
        gap = rng.uniform(0.07, 0.12)
        gain = rng.uniform(0.12, 0.25)
        for c in range(chirps):
            m = int(length * rate)
            phase = 0.0
            chirp = []
            for i in range(m):
                u = i / m
                phase += TAU * (top + (low - top) * u) / rate
                chirp.append(math.sin(phase) * math.sin(math.pi * u) ** 2)
            add(out, start + int(c * (length + gap) * rate), chirp, gain, wrap=True)
    return normalise(out, 0.5)


SOUNDS = {
    "assets/sfx/engine_idle.wav": (lambda: engine(34, 3), RATE),
    "assets/sfx/engine_mid.wav": (lambda: engine(86, 4), RATE),
    "assets/sfx/engine_high.wav": (lambda: engine(146, 6), RATE),
    "assets/sfx/horn.wav": (horn, RATE),
    "assets/sfx/crash.wav": (crash, RATE),
    "assets/sfx/thud.wav": (thud, RATE),
    "assets/sfx/load.wav": (load, RATE),
    "assets/sfx/deliver.wav": (deliver, RATE),
    "assets/sfx/lost.wav": (lost, RATE),
    "assets/sfx/whistle.wav": (whistle, RATE),
    "assets/sfx/bell.wav": (bell, RATE),
    "assets/music/theme.wav": (music, RATE),
    "assets/ambience/town.wav": (ambience, AMBIENCE_RATE),
}


if __name__ == "__main__":
    root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(".")
    for name, (make, rate) in SOUNDS.items():
        target = root / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(wav(make(), rate))
        print(f"wrote {target} ({target.stat().st_size} bytes)")
