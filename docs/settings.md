# Settings

Every pipeline option is declared once, in burrow-settings' schema
([crates/burrow-settings/src/schema.rs](../crates/burrow-settings/src/schema.rs)):
its name, type, allowed values, default, help, the part of the pipeline that
reads it (`owner`), and how a change takes effect (`deploy`: it's set in the
machine's flake; `live`: it's kept in a file its readers read again at their
next decision). This is the first slice of
[#655](https://github.com/FieldmouseWorks/burrow/issues/655): the read side.
Repository admission is the first option moved to the independent write door and runtime overlay described below. Other options remain in their existing homes.

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
| `proj.maxJobs`, `pipelineSlots`, `maxJobsPerProject`, `codex.slots`, `usageTrickle`, `usage.paid` | `burrow.proj` in the flake |
| `proj.inbox.triage.drain`, `drainJobs`, `atOnce` | `burrow.proj.inbox.triage` in the flake |
| `autoland.rounds`, its waits and limits, `repos` (per-repo overrides) | `burrow.autoland` in the flake |
| `autoland.reviewers.*` | `burrow.autoland.reviewers` in the flake |
| `settings.allowed` | `burrow.settings` in the flake |
| `settings.repositoryOverrides`, `settings.pipelineUntil` | the one runtime settings overlay |
| `proj.roles`, `proj.models`, `proj.offeredModels`, `proj.engineSetups` | `settings/engines.json`, written through the settings door and logged |
| `proj.usageHold` | proj's usage record, `.usage.json` in its jobs dir |

Before the engine store exists, engine readers parse the legacy
`~/.config/proj/{roles,models,roles.d,roles.previous}` state without writing.
The first writer imports it under both the settings lock and the legacy
engine lock. All installed writers share that migration configuration.
The old files and admission overlay stay compatible with a rolled-back
generation. An unfinished legacy switch leaves requests open for recovery
with the previous generation. Fresh, cached and adopted completion receipts
are synced before the engines door acknowledges them.

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

## Automated work settings

`burrow.settings.allowed` is the tracked allow list, set by the importing
machine's flake. It defaults to empty: new repos cannot drain issues or
start automated inbox, steward fix, judge or proof jobs until opted in.
Triage and its planner sessions can sort issues in unlisted repositories;
offering planned children and implementation stay gated. An explicit runtime
pause or scoped exclusion parks every role, including sorting and manual work.
The steward also holds their settled PRs. Existing watchdog snapshots are
gated at the reader, so a stopped publisher cannot turn a pause into repair
work or an owner escalation.

The reusable `burrow-settings` crate reads that starting policy and one
runtime overlay (`settings/overlay.json` under `burrow.stateDir`). Every
admission reloads it; malformed or unreadable settings stop admission.
The host's `settings` command and `proj settings` share the write door:

```
settings show
settings pause OWNER/REPO
settings allow OWNER/REPO
proj usage hold UNIX_TIMESTAMP --only pipeline
proj usage back
```

Pipeline scope intersects the effective allow list with `pipelineRepos`
(including `leadRepos`). It expires at the deadline, or
ends with `usage back`. A plain usage hold retains its existing engine
budget behavior. Runtime allow/pause changes persist across deployments;
`settings show`, `proj status` and the probe's `queue` answer report the
current scope, allowed repositories, and paused repositories. Every
unlisted repository, including a newly discovered one, is paused.

To keep the off switch available when pipeline services are stopped,
enable `burrow.settings.door` and set its `repo` and authorized `authors`.
An issue with its configured label (default `settings`) takes one command
line from above, omitting the `settings` prefix. Use `pipeline TIMESTAMP`
and `back` for scoped mode. The separate `burrow-settings-door` unit checks
these requests independently of the inbox, steward, CI, updater and event
dispatcher. It authenticates the opener and every editor through the shared
issue door. Repeated deliveries cannot replay an old allow over a newer
host pause. It polls independently so it remains reachable during a
pipeline outage; no paid agent handles these requests.

Writes are serialized, logged with actor and timestamp, synced and
published atomically. Readers never write their own overlay. This is the
first option set in `burrow-settings`; other operational settings join the
same tracked document, loader and overlay rather than acquiring another
store.

A pause vacates running turns, native reviews and proofs through an independent settings
reconciler. It kills their complete service groups, releasing job, Codex,
proof and inherited heavy locks. Queued turns, questions and awaits are
parked without holding admission capacity or producing stall alerts.
Job scratch, worktrees, native sessions, conversations and proof checkpoints
remain. On release, queued turns return at their original order and priority;
interrupted turns and reviews continue their native sessions, and proofs
retain completed checks. Interrupted trusted-tools copies are archived before
checks get a fresh copy from the configured origin branch. Native review queue entries keep their age and
priority while releasing reviewer capacity.
Question and await deadlines advance by the parked duration. The overlay path
wakes reconciliation immediately, scope expiry books its own wake, and a
short recovery timer handles a machine restart or interrupted reconciliation.
Turn completions and inbox admission changes also wake control directly.

Automation binds its repository identity in host-owned control state before
launch. A missing identity fails closed; deliberately local automated work
uses `proj run --local-work`. A worker cannot evade policy by changing its
project or role metadata, or by making its Git remote unreadable.
