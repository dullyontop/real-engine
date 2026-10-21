# Valley

The M3 example (`docs/M3_PLAN.md`, WP28): a procedural valley to walk and
drive through. CI checks it with `verify-valley`.

You start on the valley floor next to a car. Walk with WASD (or the left
stick), look with the mouse (or the right stick), jump with Space. Press E
next to the car to get in: W and S are throttle and brake (S reverses once the
car has stopped), A and D steer, Space is the hand brake. E again gets you out.

```sh
real run --project examples/valley
real verify --project examples/valley
```

## Layout

- `graphs/valley.graph.json`: the terrain, as a ProcGraph. Broad fBm hills
  plus inverted ridged noise, whose crest lines become the valley floors; a
  curve flattens the floor and steepens the slopes. Trees are scattered on
  gentle ground and rocks on steeper ground. Every node streams (ADR-0042),
  so the same graph runs as a streamed world.
- `scenes/main.scene.json`: the valley source, the hero, the car, the camera
  and the sun.
- `models/walker.glb`: the hero, a skinned figure (skin `Walker`, eleven
  joints) with the looping clips `idle` and `walk`. `tools/make_walker.py`
  writes it (standard-library Python, the same bytes on every run); run it
  from this directory after changing the figure.
- `scripts/game/player.luau`: walking with `physics.move_character`, the
  body's facing and its `AnimPlayer` clip, and getting in and out of the car.
- `scripts/game/car.luau`: the car's driver input through `physics.drive`.
- `scripts/game/camera.luau`: the orbit camera, on the hero or the car.
- `scripts/game/game.luau`: the HUD line.
- `tests/replays/`: `walk` (walk, strafe, turn the camera, jump) and `drive`
  (walk to the car, get in, drive along the valley, brake and reverse), each
  run twice by `real verify` with the same `state_hash`, with goldens for
  Vulkan (lavapipe) and GL.

Everything here is made by the engine's generators or by the script in
`tools/`; there are no third-party assets.

## Not yet

- The valley is a fixed 512 m square. A follow-up makes the source `Streamed`
  with a `[stream]` section and adds a long walk across cell borders, once
  the app runs the streaming manager (WP23).
- Post-processing is off (the defaults) until native projects can set it
  (WP20); shadows use the renderer's defaults.
- Erosion, roads, an animation graph and audio buses come as WP23, WP24, WP21
  and WP22 land them.
