# Enum data

Each file here, `enums/<area>.json`, lists Roblox-style enum types in the
`real-engine/compat-enums` format (`schemas/compat-enums.schema.json`). The
files are built into `re_compat` at compile time; adding a file needs no Rust
change.

```json
{
  "$schema": "../../../schemas/compat-enums.schema.json",
  "format": "real-engine/compat-enums",
  "version": 1,
  "enums": [
    {
      "name": "RunContext",
      "items": [{ "name": "Legacy", "value": 0 }, { "name": "Server", "value": 1 }],
      "status": "missing",
      "doc": "Where a script runs."
    }
  ]
}
```

Rules:

* An enum type is declared in one file only. Item names and item values are
  unique within the type; items are listed in value order.
* `status` says how far scripts can use `Enum.<Name>` (`implemented`,
  `partial` with a `note`, `stub-error`, `missing`), as for class members.
* Names and numbers are facts about the API. Every `doc` is written for this
  repository; no vendor prose is copied.
* A class property with `own` storage and type `Enum.<Name>` stores the item's
  name, so the enum must be declared here.
* `coverage.json` lists every enum. After changing a file, run
  `cargo xtask coverage`.

`core.json` declares `RunContext`, which the scene conventions need to choose a
script's context. `roblox.json` declares every other enum type of the Roblox
API, with the item names and numbers of the public API dump of Roblox client
0.743.0.7431133. Items the API marks deprecated carry `deprecated`;
the `doc` lines are written for this repository. Scripts reach all of them as
`Enum.<Name>` (`src/enums.rs`). To add an enum or item, edit the file by hand
and keep the items in value order.
