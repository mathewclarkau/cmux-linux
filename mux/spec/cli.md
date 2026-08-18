# CLI Surface

The generated CLI is `cmux <verb> ...`. The current checked-in binary also has TUI server modes; this file specifies the future generated command verbs that map 1:1 to `commands.md`.

## Global Conventions

### Socket Resolution

The CLI resolves the target session in this order:

| Priority | Source                                                       |
| -------- | ------------------------------------------------------------ |
| 1        | `--socket <path>`                                            |
| 2        | `CMUX_MUX_SOCKET`                                            |
| 3        | `--session <name>` using `$TMPDIR/cmux-<uid>/<session>.sock` |
| 4        | default session `main` using the default socket path         |

`--session` and `--socket` are global flags and may appear before or after the verb.

### Output Modes

`--json` prints the exact command result schema from `commands.md`. For stream verbs, `--json` prints one event object per line.

Human output is stable, greppable, and minimal. It must not include colors, tables with box drawing, progress spinners, or localized prose. Commands that mutate state usually print nothing on success. Create commands print the new surface id. Text extraction commands print the extracted text exactly.

### Exit Codes

| Code | Meaning                                                                              |
| ---- | ------------------------------------------------------------------------------------ |
| `0`  | Command succeeded                                                                    |
| `1`  | Server returned `ok:false` or a stream ended with a command-level error              |
| `2`  | CLI usage error, invalid flags, or invalid local argument shape                      |
| `3`  | Connection error, missing socket, auth failure, or transport failure before response |

### Stdin

`send` reads stdin when neither `--text` nor `--bytes` is supplied. Stdin is read to EOF and sent as the `text` field.

Future commands may opt into stdin only when their command block says so. By default commands do not read stdin.

### Id Arguments

Protocol v5 CLI arguments for ids are numeric. Protocol v6 accepts numeric ids and short ids for any `IdRef` parameter. Numeric-looking strings are rejected as ambiguous when short-id mode is active.

### Selector Arguments

The generated CLI requires one of `--index` or `--delta` for `select-tab`, `select-screen`, and `select-workspace`. It rejects the bare form with exit code 2 even though protocol v5 accepts it, because the bare protocol form can only be a no-op or a useless `tree-changed` emitter.

## Verb Table

| Verb                  | Status      | Required flags/args                                 | Optional flags                                                    | Human stdout                                                                                                                                                  |
| --------------------- | ----------- | --------------------------------------------------- | ----------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `identify`            | implemented | none                                                | global flags                                                      | one metadata line                                                                                                                                             |
| `list-workspaces`     | implemented | none                                                | global flags                                                      | tree lines                                                                                                                                                    |
| `get-resolved-config` | implemented | none                                                | global flags                                                      | pretty JSON chrome object                                                                                                                                     |
| `send`                | implemented | `--surface <id>`                                    | `--text <text>`, `--bytes <base64>`, `--shell <mode>`             | none                                                                                                                                                          |
| `read-screen`         | implemented | `--surface <id>`                                    | none                                                              | screen text                                                                                                                                                   |
| `vt-state`            | implemented | `--surface <id>`                                    | none                                                              | `cols=<n> rows=<n> data=<base64>`                                                                                                                             |
| `new-tab`             | implemented | none                                                | `--pane <id>`, `--cwd <path>`, `--cols <n> --rows <n>`            | surface id                                                                                                                                                    |
| `new-browser-tab`     | implemented | `--url <url>`                                       | `--pane <id>`, `--cols <n> --rows <n>`                            | surface id                                                                                                                                                    |
| `new-workspace`       | implemented | none                                                | `--name <name>`, `--cols <n> --rows <n>`                          | surface id                                                                                                                                                    |
| `new-screen`          | implemented | none                                                | `--workspace <id>`, `--cols <n> --rows <n>`                       | surface id                                                                                                                                                    |
| `split`               | implemented | `--pane <id> --dir right                            | down`                                                             | `--cols <n> --rows <n>`                                                                                                                                       | surface id               |
| `set-ratio`           | implemented | `--pane <id> --dir right                            | down --ratio <n>`                                                 | none                                                                                                                                                          | none                     |
| `set-default-colors`  | implemented | none                                                | `--fg #rrggbb`, `--bg #rrggbb`                                    | none                                                                                                                                                          |
| `get-resolved-config` | implemented | none                                                | global flags                                                      | JSON chrome (theme/tabs/sidebar/keys); used by `cmux attach --apply-local-config`/`--print-resolved-config` to layer the local overlay over the server config |
| `close-surface`       | implemented | `--surface <id>`                                    | none                                                              | none                                                                                                                                                          |
| `close-pane`          | implemented | `--pane <id>`                                       | none                                                              | none                                                                                                                                                          |
| `close-screen`        | implemented | `--screen <id>`                                     | none                                                              | none                                                                                                                                                          |
| `close-workspace`     | implemented | `--workspace <id>`                                  | none                                                              | none                                                                                                                                                          |
| `rename-pane`         | implemented | `--pane <id> --name <name>`                         | none                                                              | none                                                                                                                                                          |
| `rename-surface`      | implemented | `--surface <id> --name <name>`                      | none                                                              | none                                                                                                                                                          |
| `rename-screen`       | implemented | `--screen <id> --name <name>`                       | none                                                              | none                                                                                                                                                          |
| `rename-workspace`    | implemented | `--workspace <id> --name <name>`                    | none                                                              | none                                                                                                                                                          |
| `set-workspace-color` | implemented | `--workspace <id> --color <hex-or-preset>`          | `--colour <hex-or-empty-string>` is a back-compatible alias       | none                                                                                                                                                          |
| `set-status`          | implemented | `--icon <name>`                                     | `--workspace <id>` (active workspace when omitted)                | none                                                                                                                                                          |
| `trigger-flash`       | implemented | `--workspace <id>`                                  | `--surface <id>`                                                  | none                                                                                                                                                          |
| `resize-surface`      | implemented | `--surface <id> --cols <n> --rows <n>`              | none                                                              | none                                                                                                                                                          |
| `focus-pane`          | implemented | `--pane <id>`                                       | none                                                              | none                                                                                                                                                          |
| `select-tab`          | implemented | one of `--index`, `--delta`                         | `--pane <id>`                                                     | none                                                                                                                                                          |
| `select-screen`       | implemented | one of `--index`, `--delta`                         | none                                                              | none                                                                                                                                                          |
| `select-workspace`    | implemented | one of `--index`, `--delta`                         | none                                                              | none                                                                                                                                                          |
| `move-tab`            | implemented | `--surface <id> --pane <id> --index <n>`            | none                                                              | none                                                                                                                                                          |
| `move-workspace`      | implemented | `--workspace <id> --index <n>`                      | none                                                              | none                                                                                                                                                          |
| `scroll-surface`      | implemented | `--surface <id> --delta <n>`                        | none                                                              | none                                                                                                                                                          |
| `subscribe`           | implemented | none                                                | none in v5                                                        | event JSON lines                                                                                                                                              |
| `attach-surface`      | implemented | `--surface <id>`                                    | none                                                              | event JSON lines                                                                                                                                              |
| `wait-for`            | proposed    | `--surface <id> --pattern <regex> --timeout-ms <n>` | none                                                              | none                                                                                                                                                          |
| `run`                 | proposed    | `-- <argv...>` or `--command <cmd>`                 | `--pane <id>`, `--new-workspace`, `--cwd <path>`, `--name <name>` | surface id                                                                                                                                                    |
| `send-key`            | proposed    | `--surface <id> <key>...`                           | none                                                              | none                                                                                                                                                          |
| `copy`                | proposed    | `--surface <id> --mode screen                       | selection                                                         | scrollback`                                                                                                                                                   | none                     | text            |
| `ids`                 | proposed    | none                                                | `--kind workspace                                                 | screen                                                                                                                                                        | pane                     | surface`        | id lines |
| `notify`              | proposed    | `--title <title> --body <body>`                     | `--level info                                                     | warning                                                                                                                                                       | error`, `--surface <id>` | notification id |
| `detect-agent`        | implemented | `--surface <id>`                                    | none                                                              | `<surface> <agent> <confidence> <evidence>` (issue #78 ambient detection; `agent_name` also surfaces per-tab in `list-workspaces`)                            |
| `detect-agents`        | implemented | none                                                | none                                                              | one `<surface> <agent>` row per pane (the issue's `agent detect-batch`)                                                                                        |
| `agent-pattern-add`    | implemented | `--name <name> --pattern <marker>`                  | `--kind process\|screen`, `--confidence high\|medium\|low`, `--case-insensitive`; noun form `cmux agent-pattern add <name> ...`                                  | none (prints the registered pattern with --json)                                                                                                                |
| `agent-pattern-list`   | implemented | none                                                | noun form `cmux agent-pattern list`                              | one `<name> <kind> <confidence> <pattern>` row per pattern (bundled + user)                                                                                    |
| `agent-pattern-remove` | implemented | `--name <name>`                                     | noun form `cmux agent-pattern remove <name>`                      | none                                                                                                                                                           |
| `list-agents`         | implemented | none                                                | `--surface <id>`, `--state <state>`                               | agent lines                                                                                                                                                   |
| `report-agent`        | implemented | `--state <state>`                                   | `--surface <id>` (or `$CMUX_MUX_SURFACE`), `--source <src>` (default socket), `--agent-session <id>`, `--agent <name>`, `--message <text>`                    | none                     |
| `agent-read`          | implemented | `--target <name-or-id>`                             | `--source visible/recent/recent-unwrapped`, `--lines <n>`         | pane text                                                                                                                                                     |
| `agent-send`          | implemented | `--target <name-or-id> --text <text>`               | `--shell auto/fish/bash/zsh/sh/nu/raw`                            | none                                                                                                                                                          |
| `wait-agent-status`   | implemented | `--target <name-or-id> --status <state> --timeout <ms>` | none                                                          | pane text                                                                                                                                                     |


## Plugin Verb Group

`cmux plugin` is a local verb group (no control-socket traffic). It manages
`cmux-plugin.toml` manifests and a small JSON registry under the cmux data
directory (`$XDG_DATA_HOME/cmux`, or `~/.local/share/cmux` by default).

NOT IMPLEMENTED by this group (deferred to a follow-up PR): plugin
_execution_ (proxying `cmux <plugin-name> <verb>` calls to a running
plugin process, WASM/WASI sandboxing, the permission model). The verbs
below only manage manifest state. Do not read them as implying execution.

Manifest file `cmux-plugin.toml`:

```toml
[plugin]
name = "pifactory-fleet"        # required, non-empty, single path component
entry = "bin/fleet.wasm"         # required, non-empty; stored verbatim,
                                 # not resolved or validated as executable
verbs = ["deploy", "rollback"]   # required, non-empty; stored verbatim,
                                 # not proxied to anything yet
```

| Subcommand                            | Required args     | Optional flags         | Human stdout                                         |
| ------------------------------------- | ----------------- | ---------------------- | ---------------------------------------------------- |
| `cmux plugin list`                    | none              | `--json`, global flags | one line per plugin (`<name> <enabled                | disabled> <entry> <verb,verb>`), or `no plugins installed` when empty |
| `cmux plugin install <manifest-path>` | `<manifest-path>` | none                   | `installed plugin <name> from <path>`                |
| `cmux plugin uninstall <name>`        | `<name>`          | none                   | `uninstalled plugin <name>`                          |
| `cmux plugin enable <name>`           | `<name>`          | none                   | `plugin <name> enabled` (or `... already enabled`)   |
| `cmux plugin disable <name>`          | `<name>`          | none                   | `plugin <name> disabled` (or `... already disabled`) |

Exit codes follow the global convention: `0` success, `1` command error
(missing/malformed manifest, name collision, unknown plugin), `2` usage
error (missing/extra positional argument, unknown subcommand).

## Agents Verb Group

`cmux agents` is a local verb group (no control-socket traffic). It manages
hooks for the six registered agents: `claude`, `antigravity`, `codex`,
`aider`, `pi`, and `grok`.

| Subcommand                           | Required args    | Optional flags            | Human stdout                                                                                                   |
| ------------------------------------ | ---------------- | ------------------------- | -------------------------------------------------------------------------------------------------------------- |
| `cmux agents list`                   | none             | `--global`                | header plus one tab-separated row per agent: name, status, version, last-installed epoch seconds, install path |
| `cmux agents install --all`          | `--all`          | `--uninstall`, `--global` | one result per agent; all agents are attempted even after a failure                                            |
| `cmux agents install --only <agent>` | `--only <agent>` | `--uninstall`, `--global` | one result for the selected agent                                                                              |

`--global` is forwarded to each agent installer. `--uninstall` removes the
managed hook instead of installing it. Install returns exit code `1` when any
agent fails after all selected agents have been attempted. Unknown agents and
invalid flag combinations return exit code `2`.

## Worked Examples

1. Identify a session:

```bash
cmux --session main identify
```

2. Create a workspace and capture the surface id:

```bash
surface=$(cmux new-workspace --name build)
```

3. Send text from an argument:

```bash
cmux send --surface "$surface" --text "cargo test"$'\r'
```

4. Send a script from stdin:

```bash
printf 'printf "ready\\n"\r' | cmux send --surface "$surface"
```

5. Wait for a prompt, then send a command:

```bash
cmux wait-for --surface "$surface" --pattern 'ready' --timeout-ms 5000
cmux send --surface "$surface" --text "echo ok"$'\r'
```

6. Run a tool in a new tab and poll the screen:

```bash
surface=$(cmux run --name server -- python3 -m http.server)
until cmux read-screen --surface "$surface" | rg -q 'Serving HTTP'; do
  sleep 0.2
done
```

7. Split a pane and resize the split:

```bash
new_surface=$(cmux split --pane 2 --dir right)
cmux set-ratio --pane 2 --dir right --ratio 0.65
```

8. Subscribe to events and react to bells:

```bash
cmux subscribe |
  jq -rc 'select(.event == "bell") | .surface' |
  while read -r surface; do
    cmux notify --title "Bell" --body "Surface $surface rang" --surface "$surface"
  done
```

9. Watch agent states from a shell script:

```bash
cmux subscribe |
  jq -rc 'select(.event == "agent-state-changed") | select(.state == "blocked")' |
  while read -r event; do
    surface=$(jq -r '.surface' <<<"$event")
    cmux notify --title "Agent blocked" --body "Surface $surface needs attention" --level warning --surface "$surface"
  done
```

10. Set a workspace colour or status icon:

```bash
cmux set-workspace-color --workspace 4 --color blue
cmux set-status --workspace 4 --icon robot
cmux set-status --icon '🔍'  # active workspace
cmux workspace-color "Production Line" purple
```

The `workspace-color` positional shorthand updates the named workspace and creates it when missing. Named colour presets are `red`, `orange`, `yellow`, `green`, `blue`, `purple`, `pink`, `cyan`, and `grey`/`gray`.

11. Use short ids when protocol v6 is available:

```bash
sid=$(cmux ids --kind surface | awk 'NR == 1 {print $3}')
cmux send-key --surface "$sid" enter
```
