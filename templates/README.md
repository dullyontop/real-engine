# Templates and recipes

Project templates and generator recipes, as data (ADR-0046). The engine
embeds this folder at build time (`crates/re_agent/build.rs`), so a new file
here needs no code change.

- `real new DIR --template NAME [--set KEY=VALUE]... [--check]` makes a
  project from a template. `real new --list-templates [--json]` lists them.
- Over MCP, `template.list` and `recipe.list` list them, and `recipe.apply`
  runs a recipe on the open project.

## Templates: `<name>/template.toml`

Each template is a folder holding its files and a `template.toml`:

| Key | Meaning |
| --- | --- |
| `name` | The folder's name. |
| `summary` | One sentence saying what the project starts as. |
| `params` | The parameters, besides `name` (every template's project name). |
| `render` | Files whose `{{param}}` markers are replaced. Other files are copied as they are. |
| `folders` | Empty folders the project starts with. |
| `recipes` | Recipes applied after the copy: `{ recipe, args }`. |
| `checks` | What a new project must pass (`real new --check` and the tests). |
| `generator` | A built-in generator that writes the files instead. Only `roblox-place` exists. |

A template folder outside this one works too: `real new DIR --template
path/to/folder`.

## Recipes: `recipes/<name>.toml`

A recipe is a list of `steps` (`{ command, args }`). Each step must be a
`read` or `edit` command; anything destructive is refused (`RE8022`). A
recipe also has `params` and `checks`. `recipe.apply` is atomic by default:
it takes a checkpoint first and restores it when a step or check fails.

## Parameters

Each parameter has a `name`, a `type` (`string`, `int`, `float`, `bool` or
`color`), an optional `default` (without one, the parameter is required),
`min`/`max` for numbers, and a `doc` line.

Markers:
- `{{param}}` is replaced by the parameter's text.
- `{{param|toml}}` and `{{param|json}}` quote the text for that format.
- In recipe arguments, a string that is exactly `"{{param}}"` takes the
  parameter's typed value, such as a number or a colour array.
- A marker is a parameter name only, so Luau tables such as `{{1, 2}}` are
  left alone.

## Checks

Each check sets exactly one kind:

| Check | Holds when |
| --- | --- |
| `verify = "ok"` | Every `real verify` stage passes. |
| `verify_stage = "compile"` | That stage passes. |
| `command = "query"` | A `read` command's result, with optional `args`, matches at `pointer`. Match with one of `equals`, `at_least` and `at_most` (one or both), `near` with `tolerance`, or `contains`. |
| `diff_stage = "boot"` | That stage changed (`changed = false`: it did not) since the recipe's checkpoint. Atomic recipes only. |
| `no_regressions = true` | No stage that passed before the recipe fails after it. Atomic recipes only. |

How a `command` check matches the value at `pointer`:
- `equals`: the same JSON value; numbers compare as numbers, so `1` equals
  `1.0`.
- `at_least`, `at_most`: a number in that range, ends included. Give one or
  both.
- `near` with `tolerance`: a number within `tolerance` of `near`
  (`near = 32.0`, `tolerance = 0.5` holds from 31.5 to 32.5). `tolerance`
  is required, and 0 asks for the exact number.
- `contains`: a string that contains the text, or an array that holds the
  value.

A number check on something that is not a number fails, and says what it
found.

The tests in `crates/re_cli` create a project from every template and run
its checks, and apply every recipe over MCP.
