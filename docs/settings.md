# Settings

Every pipeline option is declared once, in burrow-settings' schema
([crates/burrow-settings/src/schema.rs](../crates/burrow-settings/src/schema.rs)):
its name, type, allowed values, default, help, the part of the pipeline that
reads it (`owner`), and how a change takes effect (`deploy`: it's set in the
machine's flake; `live`: it's kept in a file its readers read again at their
next decision). This is the first slice of
[#655](https://github.com/FieldmouseWorks/burrow/issues/655): the read side.
Repository admission and each role's engine, model and effort have moved to the write door and runtime overlay described below. Other options remain in their existing homes.

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
| `proj.roles`, `proj.models`, `proj.engineOffers`, `proj.engineSetups` | the one runtime settings overlay ([below](#engines-models-and-efforts-per-role)) |
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
- The live values are read as proj's turns read them, so an overlay a turn
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

## Engines, models and efforts per role

Which engine, model and effort runs each kind of job are live settings in
the same overlay, changed through the same door and logged the same way:

```
proj settings role ROLE ENGINE [over PCT ENGINE]   which engine runs ROLE
proj settings role ROLE unset                      back to the declared default
proj settings model ROLE ENGINE MODEL|- [EFFORT|-] its model and effort on ENGINE
proj settings model ROLE ENGINE unset
proj settings offer ENGINE models|efforts WORD...  what ENGINE offers
proj engines [save NAME [--replace] | use NAME | use back]
```

A role key is `ROLE`, `ROLE@OWNER/REPO`, `*` (every role) or
`*@OWNER/REPO`; the closest one wins: a named role over `*`, a repo's key
over the bare one, the repo matched case-insensitively. Engines are
`claude` and `codex`. `over PCT ENGINE` moves a turn to that engine when
its own is short: any window on its meter (the five-hour session or the
week) at PCT with no credits to carry on, or, for Claude, the machine
already out of Claude's usage. It moves only when the other engine's meter
has room: credits, or no window used up. Missing readings, or readings an
hour old, move nothing. `burrow.usageReset` supplies both meters. Each
engine keeps its own conversation when a role moves between them. A
steward judge always runs on Claude.

Nothing is an implicit fallback: the schema declares each default and
`proj settings show` shows it as one. Every role runs on Claude (`*`)
but the planner, on Codex, which needs an explicit model to run on Claude;
a role with no model runs at its engine's own (`* claude`, `* codex`,
model and effort null): for Claude, the job user's
`~/.claude/settings.json` `model` and the machine's `burrow.claude.effort`;
for Codex, the job user's `.codex/config.toml`. The tuning for the engine a
turn ends on applies, overflow and the judge's Claude included. New sandbox
guests (`agent new`, `proj run --vm`) copy the user's Claude `model` and its
`env.CLAUDE_CODE_SUBAGENT_MODEL` and `env.CLAUDE_CODE_SUBAGENT_MODEL_FORCE`
at creation.

The door refuses a model or effort its engine doesn't offer
(`proj.engineOffers`). Each engine offers the efforts its CLI takes by
default and no model until `offer` lists them, and an offer a role is set
to can't be withdrawn under it. Model ids live only in the overlay, never in
a tracked file: which model does what is the machine's business.

A saved engine setup is a named set of these same values, `proj.roles` and
`proj.models` as set over the defaults, so it can't disagree with the
settings in force. `engines save NAME` saves what's in force (`--replace`
to overwrite; names are lowercase letters, digits and hyphens, and `back`
is reserved), `engines use NAME` puts exactly its values in force, after
checking them against the offers, and changes nothing else, and
`engines use back` returns to what the last switch replaced. The active
setup is the one whose values are in force; a change after it leaves none
active until one matches again. `proj engines` lists them, with each role's
engine, and never a model id: the [engines door](front-door.md#engines-with-no-agent)
answers with it.

`proj check-config` reads the role settings as each turn does; the update's
health check runs it as the job user after every switch. proj's old files
(`~/.config/proj/roles`, `models`, `roles.d/`) moved into the overlay once,
by `proj settings migrate` (the `proj-settings-migrate` unit), which renames
them `NAME.migrated`; nothing reads them, and check-config fails while one
is back.
