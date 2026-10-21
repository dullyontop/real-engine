# Evaluation suites

`real agent eval SUITE` runs agent tasks against sample projects and
scores them with `real verify` (ADR-0046). The engine never calls a model.

- `starter/` uses the `native` and `terrain` templates. It is fast, and the
  end-to-end tests run it over both transports.
- `island/` uses `examples/island`.

```sh
real agent eval crates/re_agent/evals/starter                      # reference calls, over HTTP
real agent eval crates/re_agent/evals/island --transport stdio
real agent eval crates/re_agent/evals/starter --agent claude --runs 3
```

## Tasks

A suite is a directory of task files, one task per `*.toml`:

```toml
prompt = "Add a box that falls onto the ground."  # what a person would ask
project = { template = "native" }                 # or { template = "terrain", params = { seed = 3 } },
                                                  # { example = "island" }, { path = "dir" }
budget = { tool_calls = 20, seconds = 300 }

[[checks]]                                         # as in templates/README.md
no_regressions = true

[[reference]]                                      # calls that solve the task
command = "recipe.apply"
args = { recipe = "falling-box" }
```

- `{ example = "NAME" }` is found as `examples/NAME` in a folder above the
  suite.
- `{ path = "dir" }` is relative to the suite.
- Checks are the template checks. `no_regressions` and `diff_stage`
  compare with `real verify` on the project before the run.

## What a run does

For each task and run:

1. The harness copies the project into
   `--out/<suite>/<task>/run-<n>/project` and writes the agent policy that
   `real agent init` writes.
2. It runs `real verify` there as the baseline.
3. The driver acts on the project:
   - **reference** (no `--agent`): the harness serves `real mcp` and sends
     the task's `[[reference]]` calls as an MCP client.
   - **command** (`--agent NAME`): the harness runs your agent.
4. The harness reads the transcript and runs `real verify` again. It then
   scores the run with the task's checks.

A run passes when every check holds, the tool calls stay within the budget,
and the driver finished in time without an error.

Each run directory holds:

| File | Contents |
| --- | --- |
| `transcript.jsonl` | One line per tool call, written by the server. |
| `agent.stdout`, `agent.stderr` | The agent's own output (command driver only). |
| `server.stderr` | The server's own output. |
| `verify.json`, `verify-diff.json` | The final verify report, and what changed since the baseline. |
| `score.json` | The run's score. |

`<suite>/summary.json` counts the runs that passed. The command exits 0
only when every run passed.

## Agents

An agent is any program you configure in `eval.toml`. The harness looks for
it in `./eval.toml`, `$XDG_CONFIG_HOME/real/eval.toml`,
`~/.config/real/eval.toml` and `%APPDATA%\real\eval.toml`:

```toml
[agents.claude]
command = ["claude", "-p", "{prompt}", "--mcp-config", "{mcp_config}", "--allowedTools", "mcp__real-engine"]
env = { }
timeout_s = 600
```

The harness replaces three placeholders in `command`:

- `{prompt}`: the task's prompt;
- `{mcp_config}`: a Claude Code style `.mcp.json` that points at the server;
- `{project}`: the project copy, which is also the agent's working
  directory.

The environment gets `REAL_EVAL_PROMPT`. Over HTTP it also gets
`REAL_MCP_URL` and `REAL_MCP_TOKEN`. Over stdio, the config starts
`real mcp` itself.

The model, keys and provider all belong to that command. A task never names
an agent.
