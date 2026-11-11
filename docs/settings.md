# Settings

Every pipeline option is declared once, in burrow-settings' schema
([crates/burrow-settings/src/schema.rs](../crates/burrow-settings/src/schema.rs)):
its name, type, allowed values, default, help, the part of the pipeline that
reads it (`owner`), and how a change takes effect (`deploy`: it's set in the
machine's flake; `live`: it's kept in a file its readers read again at their
next decision). This is the first slice of
[#655](https://github.com/FieldmouseWorks/burrow/issues/655): the read side.
Options are still written where they live today; the write door, its change
log and moving each option's home come later, on this same schema.

## Reading them

```
proj settings show           a line an option: its value, where it came from
proj settings show --json    the same as JSON, held to the schema below
proj settings schema         the schema as JSON Schema (draft 2020-12)
```

Each value says where it came from: `default` (nothing sets it: the schema's
default), `flake` (the machine's flake sets it) or `live` (a file on the
machine, named beside it). What it covers today:

| Options | Where they live now |
|---|---|
| `proj.maxJobs`, `pipelineSlots`, `maxJobsPerProject`, `codex.slots`, `usageTrickle`, `usage.paid`, `paused` | `burrow.proj` in the flake |
| `proj.inbox.triage.drain`, `drainJobs`, `atOnce` | `burrow.proj.inbox.triage` in the flake |
| `autoland.rounds`, its waits and limits, `repos` (per-repo overrides) | `burrow.autoland` in the flake |
| `autoland.reviewers.*` | `burrow.autoland.reviewers` in the flake |
| `proj.roles` | the roles file (`~/.config/proj/roles`) |
| `proj.models` | the models file beside it |
| `proj.engineSetups` | the saved setups, `roles.d/`, and the one in force |
| `proj.usageHold` | proj's usage record, `.usage.json` in its jobs dir |

The JSON Schema marks each option with `x-owner` and `x-change`, gives each
value's type and default, and requires every option once and no other: a
dashboard can be built from it alone.

## How it stays one list

- The flake's values reach `proj settings` in `/etc/burrow/settings.json`,
  rendered by [modules/settings/tracked.nix](../modules/settings/tracked.nix)
  by schema name, with whether the flake set each one (a definition above the
  option's own default) and the module's own default. proj's config names
  the file rather than holding it: those values may run proj themselves (a
  repo's `proofRuns`).
- `proj settings show` only builds its document from the schema's options
  (`burrow_settings::show::Document`): a value for a name the schema doesn't
  declare, a declared option nothing read, a value of the wrong kind, a value
  from where its option can't come, or a module's default that isn't the
  schema's, each fails it, naming the option. That last holds whether or not
  the flake overrides the option, and a derived default (`pipelineSlots`,
  `maxJobsPerProject`) is worked out from the machine's own values by the
  schema's formula.
- The live files are read with the parsers proj's turns use, so a line a turn
  would refuse fails the show too.
- `checks/settings.nix` runs it on a fixture machine with values from its
  flake and every live file, and validates the result against the generated
  schema with a validator of its own (check-jsonschema).
