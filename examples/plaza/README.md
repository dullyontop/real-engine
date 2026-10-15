# Plaza

A small game written in Roblox-style Luau for Real Engine's compatibility
layer (`[compat] api = "roblox"`). CI checks the layer with it
(`verify-plaza`).

Coins spawn around a grass plaza from a seed. Touching one collects it, and
it comes back a few seconds later. The counter in the top left shows your
coins. The blue stand sells a speed boost through a `ProximityPrompt`, and
paying for the door slides it open with `TweenService`. A spinner turns on
`Heartbeat`.

```sh
real run --project examples/plaza
real verify --project examples/plaza
```

## Layout

- `scripts/ServerScriptService/Main.server.luau`: the server. It makes the
  remotes, builds the plaza, keeps each player's coins in a `Coins`
  attribute, runs the shop and the door, and adds the test hooks.
- `scripts/ServerScriptService/World.luau`: the ground, the spawn, the
  spinner, the door and the shop stand.
- `scripts/ServerScriptService/Coins.luau`: the tagged coins and their
  `Touched` pickup, with a `Taken` attribute as the debounce.
- `scripts/ReplicatedStorage/Shared.luau`: settings both sides read.
- `scripts/StarterPlayer/StarterPlayerScripts/Hud.client.luau`: the coin
  counter and the shop's messages, a `ScreenGui` built in code.

The character and camera come from the engine's default player scripts.

## Test hooks

`ServerStorage.TestHooks` holds `BindableFunction`s for `play.run`'s
`invoke` step: `Coins(player)`, `Give(player, n)`, `Buy(player, item)` (item
`"speed"` or `"door"`), `CoinParts()`, `Position(player)` and
`DoorHeight()`. For example:

```sh
real do play.run --project examples/plaza --args \
  '{"frames": 30, "steps": [{"invoke": {"path": "ServerStorage.TestHooks.Give", "args": ["Player1", 5]}}]}'
```
