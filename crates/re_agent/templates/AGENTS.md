# Working on this Real Engine project

This is a game project for Real Engine. You change it through the engine's
**commands**, not by editing scene files by hand. Every command is an MCP tool
of the `real-engine` server (`real mcp`), named with `_` for `.`
(`entity.spawn` is the tool `entity_spawn`). The server refuses anything the
agent policy does not allow, and reports errors as diagnostics with a code
(`RE####`). Use the MCP tools rather than a shell: they are what the policy
grants you.

## The loop

1. Look first: `describe` lists every component, generator, graph node kind,
   project script and command; `query` finds entities; `entity.get` shows
   one; `command.describe` gives a command's full input schema.
2. Preview: every command that changes something accepts `"dry_run": true`
   and returns the operations it would apply, without applying them. Dry runs
   are allowed whatever the tier (budgets still apply).
3. Change: make the edit. Each scene command is one transaction; `undo` and
   `redo` step through them. File commands (`script.write`, `file.delete`)
   are not in the undo history: `checkpoint.restore` brings files back.
4. Check: `verify` runs the project's checks, `screenshot` renders a frame
   (the PNG path is in the result), and `play.run` steps a copy of the game
   for some frames and reports what happened. None of them change the project.
   `verify.diff` runs verify and lists only what changed since your latest
   checkpoint: stages that now fail or pass, new or fixed diagnostics, and
   changed values. Take a checkpoint with `checkpoint.create` when the
   project is in a state you want to compare against.
5. If it went wrong: `undo`, or `checkpoint.restore` to return the scene and
   every file you touched to the start of your session. A restore saves the
   state it replaces as a new checkpoint (its id, `saved`, is in the result);
   to take a restore back, restore `saved`, not `undo` (which would revert
   only the scene).

An error's code explains itself: `real explain RE2005`.

## Editing the scene with `scene.apply`

`scene.apply` applies a list of operations as **one atomic transaction**: if
any operation fails, nothing changes, and one `undo` reverts all of them.
Prefer it for any change of more than one step. Each operation is a JSON
object with an `op` tag:

| `op` | Fields | Does |
|---|---|---|
| `spawn` | `id`?, `name`?, `parent`?, `index`?, `components`? | Create an entity (an id is generated when omitted) |
| `despawn` | `id` | Remove an entity and its descendants |
| `set_field` | `id`, `component`, `field`, `value` | Set one field |
| `add_component` | `id`, `component`, `fields`? | Add a component (omitted fields take defaults) |
| `remove_component` | `id`, `component` | Remove a component |
| `reparent` | `id`, `parent` (`null` for a root), `index`? | Move an entity |
| `rename` | `id`, `name` | Change the display name |
| `set_override` | `source`, `target`, `component`?, `field`?, `value`?, `deleted`? | Keep a change to a generated entity across regenerations |

Values use the file form: vectors and colours are arrays (`[x, y, z]`,
`[r, g, b, a]`), enums are strings, and entity references are entity ids.

```json
{
  "label": "Add a lit rock",
  "ops": [
    { "op": "spawn", "id": "rock", "name": "Rock",
      "components": { "Transform": { "translation": [0, 1, 0] } } },
    { "op": "set_field", "id": "rock", "component": "Transform", "field": "scale", "value": [2, 2, 2] },
    { "op": "rename", "id": "rock", "name": "Big rock" }
  ]
}
```

Entities made by a procedural generator have ids `<source>/<key>`. Change
them through the source's parameters (`graph.set_param`, then
`graph.regenerate`), or with `set_override` when one generated entity must
differ; direct structural edits of generated entities are refused (`RE2003`).

## Scripts

`script.write` writes a whole Luau file, and only if it compiles: with a
syntax error nothing is written and the result carries the errors with their
line. Scripts live under `scripts/`. `checkpoint.restore` returns a script to its
earlier contents.

## Project tools written in Luau

The project can add its own commands: each script under
`scripts/tools/commands/` returns a table with a `name`, a `summary`, a
`tier` (`read` or `edit`), an `input` JSON Schema and a `run(args)` function
that changes the project through `command.run(...)`. They appear in the tool
list with the engine's commands, follow the same policy, and each call is one
undoable transaction. `real explain RE8015` shows the shape. A tool you write
with `script.write` is listed after the MCP server restarts.

## Reference material

The explain pages, the engine's documents and the Luau definitions
(`engine.d.luau`, and `roblox.d.luau` for the Roblox-style API) are MCP
resources (`real://explain/RE2005`,
`real://docs/ARCHITECTURE.md`, `real://luau/engine.d.luau`). Without
resources, `resource.list` and `resource.read` return the same text.

## Roblox-style projects

`compat.coverage` says which Roblox-style classes, members, enums and
libraries the engine implements (`implemented`, `partial` with the gap,
`stub-error`, `missing`); ask for one class by `name` to see its inherited
members. `scene.dump_tree` returns the instance tree with each entity's class,
attributes and tags.

`project.import` brings a Rojo-style project or a Studio loader script that is
inside the project into it. It only reports what would change until you pass
`write: true`; the write is one undoable step. It removes instances an earlier
import made that the input no longer declares, which needs `destructive`;
pass `keep_stale: true` to keep them. `play.run` plays the place with steps
(`add_player`, `say`, `invoke`, `dump_tree`) to check it.

## What you may do

`.real/agent-policy.toml` sets the most an agent may do. Every command has a
tier:

- `read`: looks, previews and checks; always allowed (and so is any dry run);
- `edit`: undoable changes inside the project; allowed when the policy's
  `max_tier` is `edit` (which `real agent init` writes);
- `destructive`: changes that can lose work (`file.delete`); only when a
  person sets `max_tier = "destructive"`.

File-writing commands may only write inside the policy's `[scopes]`, and
never in `.real/` or `.git/` or through a symbolic link. Budgets cap `play.run` frames and the size of
one transaction. A refused call returns `RE8001` (tier), `RE8002` (path) or
`RE8008` (budget) and changes nothing; tell the user what you needed instead
of working around it.
