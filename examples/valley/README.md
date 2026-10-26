# Valley

The M3 example (`docs/M3_PLAN.md`, WP28): a procedural valley to walk and
drive through. CI checks it with `verify-valley`, packs it and plays its
replays from the package in `build-package`, and compares its replays'
`state_hash` on Linux, Windows and macOS in `d2`.

The valley is a streamed world (ADR-0042): 16 km square, generated cell by
cell (256 m cells) around the hero as you go, with the `[stream]` defaults.
You start on the valley floor next to a car. The hero and the car wait until
the ground under them has loaded, and are then set down on it.

Walk with WASD (or the left stick), look with the mouse (or the right stick),
jump with Space. Press E next to the car to get in: W and S are throttle and
brake (against the way the car rolls they brake first, then drive), A and D
steer, Space is the hand brake. E again gets you out.

```sh
real run --project examples/valley
real verify --project examples/valley
real build --project examples/valley --out dist/valley
```

`real.toml` turns on mild post-processing (neutral tone mapping, a little
bloom and slightly richer colour), sets the sun's shadow cascades (four, out
to 200 m), defines the mixer and packs `models/`, which is not a default
package folder.

The mixer (`[audio]`, ADR-0041) has a bus for each kind of sound: `footsteps`
and `vehicle` under `sfx`, which also feeds a reverb send (`echo`), and
`ambience` for the wind. While the hero drives, the `in_car` snapshot turns
the wind down and muffles it, and lowers the echo.

## Layout

- `graphs/valley.graph.json`: the terrain, as a ProcGraph. Broad fBm hills
  plus inverted ridged noise, whose crest lines become the valley floors; a
  curve flattens the floor and steepens the slopes. Droplet erosion
  (`terrain.erode`, on a 4 m grid) then cuts gullies into the slopes and
  leaves sediment on the floor. Trees grow on the lower slopes, leaving the
  floor open to drive along, and rocks lie on steeper ground. Every node
  streams.
- `scenes/main.scene.json`: the valley source (`Streamed`, bounds 8000 m,
  inside the ±8 km precision limit), the hero and the car (each a `StreamFocus`,
  so the ground under a parked car stays loaded while the hero walks away),
  the camera, the sun and the wind. The car is kinematic until the ground under
  it loads; then the player script makes it a dynamic `Vehicle`. The car's
  engine sound is three looping layers, children of the car.
- `models/walker.glb`: the hero, a skinned figure (skin `Walker`, eleven
  joints) with the looping clips `idle`, `walk` and `fall`.
- `anims/walker.animgraph.json`: the hero's animation graph (ADR-0039). Its
  `speed` parameter (m/s) blends `idle` with `walk` played at six rates, from
  half the clip's own pace of 1.69 m/s up to the hero's top speed of 4 m/s, so
  the boots keep pace with the ground to within a few percent at any speed;
  while `airborne` is set, it crossfades to `fall`, and back on landing.
- `assets/`: the sounds. Footsteps (two takes), a landing, the car door,
  three engine layers (about 1000, 2800 and 5000 rpm) and wind.
- `tools/make_walker.py` and `tools/make_sounds.py` write the model and the
  sounds (standard-library Python, the same bytes on every run); run them
  from this directory after changing them.
- `scripts/game/player.luau`: walking with `physics.move_character`, the
  body's facing, the graph's parameters (`anim.set`), a footstep where a boot
  lands (read with `anim.joint`), and getting in and out of the car, with the
  door and the `in_car` snapshot. The hero gets out by a door, or else behind
  or in front, wherever there is room to stand; with none, the hero stays in.
- `scripts/game/car.luau`: the car's driver input through `physics.drive`, and
  its engine sound: the three layers, crossfaded by the engine's rpm and
  louder with more throttle.
- `scripts/game/camera.luau`: the orbit camera, on the hero or the car.
- `scripts/game/game.luau`: the HUD line.
- `tests/replays/`: `walk` (walk, strafe, turn the camera, jump), `drive`
  (walk to the car, get in, drive along the valley, brake and reverse) and
  `cross` (turn round and walk 70 s west along the valley, from the cell
  border at x = 0 across the one at x = -256 while cells load ahead).
  `real verify` runs each twice with the same `state_hash` and checks goldens
  for Vulkan (lavapipe) and GL. CI's `d2` job records every replay's
  `state_hash` checkpoints with `cargo xtask d2` on Linux, Windows and macOS,
  and `d2 compare` checks they match.

Everything here is made by the engine's generators or by the scripts in
`tools/`; there are no third-party assets.

## Not yet

- Roads: the road nodes (ADR-0043) shape whole paths, which a streamed graph
  cannot use yet (`RE4301`).
- Distant terrain: `terrain.chunks` would draw it in levels of detail, but its
  far chunks swap in when the worker pool finishes them, so screenshots would
  not repeat; the valley meshes only the loaded cells, with `terrain.mesh`.
- The engine sound changes layers, not pitch: `AudioSource` has no playback
  rate.
- There are no DX12 or Metal goldens yet, so with those backends (the
  defaults on Windows and macOS) `real verify` fails its `goldens` stage with
  `RE0510`.
