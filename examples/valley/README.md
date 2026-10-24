# Valley

The M3 example (`docs/M3_PLAN.md`, WP28): a procedural valley to walk and
drive through. CI checks it with `verify-valley`, packs it and plays its
replays from the package in `build-package`, and compares its replays'
`state_hash` on Linux, Windows and macOS in `d2`.

The valley is a streamed world (ADR-0042): 16 km square, generated cell by
cell (256 m cells) around the hero as you go, with the `[stream]` defaults.
You start on the valley floor next to a car; the hero and the car wait until
the ground under them has loaded.

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
bloom and slightly richer colour) and packs `models/`, which is not a default
package folder.

## Layout

- `graphs/valley.graph.json`: the terrain, as a ProcGraph. Broad fBm hills
  plus inverted ridged noise, whose crest lines become the valley floors; a
  curve flattens the floor and steepens the slopes. Trees grow on the lower
  slopes, leaving the floor open to drive along, and rocks lie on steeper
  ground. Every node streams.
- `scenes/main.scene.json`: the valley source (`Streamed`, bounds 8000 m,
  inside the ±8 km precision limit), the hero and the car (each a `StreamFocus`,
  so the ground under a parked car stays loaded while the hero walks away),
  the camera and the sun. The car is kinematic until the ground under it
  loads; then the player script makes it a dynamic `Vehicle`.
- `models/walker.glb`: the hero, a skinned figure (skin `Walker`, eleven
  joints) with the looping clips `idle` and `walk`. `tools/make_walker.py`
  writes it (standard-library Python, the same bytes on every run); run it
  from this directory after changing the figure.
- `scripts/game/player.luau`: walking with `physics.move_character`, the
  body's facing and its `AnimPlayer` clip, and getting in and out of the car.
- `scripts/game/car.luau`: the car's driver input through `physics.drive`.
- `scripts/game/camera.luau`: the orbit camera, on the hero or the car.
- `scripts/game/game.luau`: the HUD line.
- `tests/replays/`: `walk` (walk, strafe, turn the camera, jump), `drive`
  (walk to the car, get in, drive along the valley, brake and reverse) and
  `cross` (turn round and walk 70 s west along the valley floor, from the
  cell border at x = 0 across the one at x = -256 while cells load ahead).
  `real verify` runs each twice with the same `state_hash` and checks goldens
  for Vulkan (lavapipe) and GL; `cargo xtask d2` checks the hashes match on
  every platform. `cross`'s stick inputs were recorded from a run that
  steered towards the low ground.

Everything here is made by the engine's generators or by the script in
`tools/`; there are no third-party assets.

## Not yet

- Erosion, roads, an animation graph and audio buses come as WP23, WP24, WP21
  and WP22 land them.
- Shadows use the renderer's default cascades (`[render.shadows]` is not set).
