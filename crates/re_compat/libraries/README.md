# Library data

Each file here, `libraries/<area>.json`, lists globals that are not classes, in
the `real-engine/compat-libraries` format
(`schemas/compat-libraries.schema.json`):

* datatype tables such as `Vector3` or `CFrame`, with their members (`new`,
  `zero`, `Angles`);
* function libraries such as `task`, and global functions such as `typeof`;
* globals that are instances, such as `game`, `workspace` or `script`. These
  name a `class` and have no `members` of their own: their members are the
  class's, inherited ones included.

```json
{
  "$schema": "../../../schemas/compat-libraries.schema.json",
  "format": "real-engine/compat-libraries",
  "version": 1,
  "libraries": [
    { "name": "game", "class": "DataModel", "status": "missing" },
    {
      "name": "task",
      "status": "partial",
      "note": "No task.synchronize.",
      "members": [{ "name": "wait", "status": "implemented" }]
    }
  ]
}
```

Each global is declared in one file; member names are unique within it; a
`partial` row needs a `note`. The work package that implements a global owns
the file that declares it. After changing a file, run `cargo xtask coverage`:
the gap report reads globals as `name`, members as `name.member`, and the
members of an instance global through its class.
