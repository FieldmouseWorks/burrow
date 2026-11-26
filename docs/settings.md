# Settings

Every pipeline option is declared once, in burrow-settings' schema
([crates/burrow-settings/src/schema.rs](../crates/burrow-settings/src/schema.rs)):
its name, type, allowed values, default, help, the part of the pipeline that
reads it (`owner`), and how a change takes effect (`deploy`: it's set in the
machine's flake; `live`: it's kept in a file its readers read again at their
next decision). The [settings reference](settings-reference.md) lists them
all, generated from the schema: each option's type, allowed values, default,
owner, and where its value is kept. This is the first slice of
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
machine, named beside it).

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
  the flake overrides the option, and a derived one (the reference says
  which) is worked out from the machine's own values by the schema's
  formula.
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
runtime overlay under `burrow.stateDir` (the reference names its file). Every
admission reloads it; malformed or unreadable settings stop admission.
The host's `settings` command and `proj settings` share the write door:

```
settings show
settings pause OWNER/REPO
settings allow OWNER/REPO
settings land OWNER/REPO#N HEAD
settings landing OWNER/REPO steward|conaryd
proj usage hold UNIX_TIMESTAMP --only pipeline
proj usage back
```

`landing` says who lands the repo's PRs, the steward or conaryd
([landing.md](landing.md) owns the subject).

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

A pause vacates running turns and proofs through an independent settings
reconciler. It kills their complete service groups, releasing job, proof
and inherited heavy locks. Queued turns, questions and awaits are
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

### Handing one PR to the steward

`settings land OWNER/REPO#N HEAD` (or the door's `land OWNER/REPO#N HEAD`)
hands one PR, at its full head commit id, to the steward while its
repository stays paused: the sanctioned way for a session to have a PR in a
paused repo merged, since the steward is what merges. It is the same
authority as `allow` (the machine user, or the door's authors), scoped to
far less, and logged with its actor like every write. The handoff admits
that PR's own steward pass and its landing at that head, and nothing else:

- every landing requirement still holds: a clean review of the change on
  that head, every thread resolved, CI green, proofs passed, no hold or
  owner label, a judge's verdict where one is owed, and the train's own
  checks where the repo has one;
- whatever it still needs is paused with the repository: no Claude job
  (a fix, a judge, a proof job), no proof build or run and no review starts
  for it, so a PR missing any of them keeps waiting, its status saying so;
- another head isn't the one handed, so a push since keeps it paused; a new
  request for the new head replaces the old one;
- in a train repo it may join the train, and the train steps only while
  every PR in it is handed, so no other PR lands with it;
- it is one-shot: the steward spends it once the PR has merged, and on the
  repo's next pass after the PR has closed or the train landed it, so a
  redelivered request (a door receipt answers again without writing),
  duplicate event or restarted steward finds nothing to reuse;
- a `pause OWNER/REPO` written after it revokes every handoff in the repo.

`settings show` lists the outstanding handoffs, and `decision OWNER/REPO`
gives them (`handed`) to the steward
([admission.py](../modules/autoland/admission.py)).

Automation binds its repository identity in host-owned control state before
launch. A missing identity fails closed; deliberately local automated work
uses `proj run --local-work`. A worker cannot evade policy by changing its
project or role metadata, or by making its Git remote unreadable.

## The API credit

`proj.apiCredit` is the expiring API credit the `claude-api` engine may
spend; the [reference](settings-reference.md) gives its fields and the room
rule. It's kept in a file of its own beside the admission overlay, under the
same lock and log, so a generation from before it still reads that overlay,
and only the write door changes it:

```
settings credit BUDGET_USD RENEWS_YYYY-MM-DD MONTHLY_USD RESERVE_USD
settings credit off
```

`BUDGET_USD` is the balance read now: the write records the time as `since`,
the cycle's start, and counts spend from then. Writing the same balance again
before it renews (a repeat, or a new reserve or monthly amount) keeps the
earlier `since`, so what the cycle spent still counts.

`burrow_settings::credit` is the meter: given the setting, the time and the
turns' costs, it works out the cycle (renewals are stepped from `renews`' own
day of the month, so several missed ones still leave one month's credit),
the spend, today's allowance and whether there's room, in whole
micro-dollars. `burrow_probe::credit::meter` reads it on the machine: the
setting, and each `claude-api` turn's cost from proj's job records, decoded by
the usage answer's accounting with every Claude Code turn of a job in order,
so a session carried over from the subscription is charged only its own part.
Each run of a turn (its first and every retry) counts at its own start, from
its `burrow.run` marker, so a retry after midnight or a renewal is that
day's and that cycle's spend.
The probe's `usage` answer shows it, and proj admits every `claude-api` run
(a turn's first and each retry) through the same function, under one lock
for the machine ([`engine/api.rs`](../crates/burrow-proj/src/engine/api.rs)):
the run is given the room left (above the reserve, and of today's allowance,
whichever is less) as its `--max-budget-usd`, and its `burrow.run` marker
records that budget before the lock goes. Until its cost is recorded, the
meter counts the run at that budget, so runs going at once never spend the
same room twice, and one stopped before its result holds no more than it
was allowed until a later run of its session accounts for it. Claude Code
stops a run past its budget after the call that took it there, so a run can
go one call over: the reserve absorbs that, and the Console's spend limit on
the key's workspace is the backstop past it. Anything the meter can't read
is no room, and so is a run whose marker gives no budget (one from before
admission wrote them) until its cost is in. A new balance read from the
Console (`settings credit` with a different `BUDGET_USD`) starts counting
afresh.
`renews` is a day, so the renewed credit counts from 00:00 UTC on it: set
it to the first day the new credit is surely there.

## Engines, models and efforts per role

Which engine, model and effort runs each kind of job are live settings in
the same overlay, changed through the same door and logged the same way:

```
proj settings role ROLE ENGINE [over PCT ENGINE]... which engine runs ROLE, then
                                                   where its turns go on to
proj settings role ROLE unset                      back to the declared default
proj settings model ROLE ENGINE MODEL|- [EFFORT|-] its model and effort on ENGINE
proj settings model ROLE ENGINE unset
proj settings offer ENGINE models|efforts WORD...  what ENGINE offers
proj engines [save NAME [--replace] | use NAME | use back]
```

A role key is `ROLE`, `ROLE@OWNER/REPO`, `*` (every role) or
`*@OWNER/REPO`; the closest one wins: a named role over `*`, a repo's key
over the bare one, the repo matched case-insensitively. The engines are
`claude`, on the subscription's login, and `claude-api`, the same Claude
Code on the API credit (`proj.apiCredit` above). A steward judge follows
its role like any other.

A route is an ordered chain: `claude over 95 claude-api` runs the role on
the subscription, and on the credit once the subscription is short. Each
`over PCT ENGINE` says when to go on past the engine before it: once that
engine has used PCT percent by its fresh meter (any of Claude's windows,
or the credit's spend of its cycle's budget), or is out of usage (a hold a
refusal recorded, whatever the meter last said; for the credit, no room by
its meter or no key). The turn then takes the first engine after it that
is under its own threshold and has room by its fresh meter (the credit's
pacing and reserve included); failing that, the first that can run at all;
failing that, the first engine, whose turn waits for it as before. A
missing, unreadable or stale reading moves nothing, and no turn is moved
onto an engine that can't vouch for its room. A script decides this each
time a turn starts, never a model, and nothing is remembered: a turn goes
back to the first engine as soon as it isn't short. A run refused for
usage, or one the credit refuses, is routed again before it runs once
more, and carries on in the same conversation when the engine it goes to
shares it (both Claude engines do). A pinned engine (an issue's label)
starts the chain where the route names it, or runs alone when it doesn't.
A sandbox never goes on to the credit: it's given no key. An engine is
named once per route.

Nothing is an implicit fallback: the schema declares each default and
`proj settings show` shows it as one. Every role runs on Claude (`*`); a
role with no model runs at Claude's own (`* claude`, model and effort
null): the job user's `~/.claude/settings.json` `model` and the machine's
`burrow.claude.effort`. New sandbox
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

They're kept beside the admission overlay, in `overlay.roles.json`, under
the same lock and log, so a generation from before them still reads its
overlay after a rollback. A turn reads them once: its engine, model and
effort come from one version of them, and so does `settings show`.

`proj check-config` reads the role settings as each turn does; the update's
health checks require a durable migration after every switch. proj's old files
(`~/.config/proj/roles`, `models`, `roles.d/`) are imported once by `proj
settings migrate`. The deployed `proj-settings-migrate` service holds the old
CLI lock and the old engines door's whole-pass lock for the generation's
lifetime, including after a migration error. Its readiness means the retired
writers are fenced; the separate migration health check requires the cutover
to be durable before confirmation. New engines-door passes use a separate lock.
Rollback stops the fence and makes the legacy inputs writable again.

Every role, tuning, offer and setup mutation waits for configured legacy
inputs to migrate, so unsets and complete replacements cannot be undone by
import. Admission's off switch remains available. With no legacy source
configured, the first role mutation establishes a fresh settings store.

Recovery finishes a journalled switch in the legacy inputs, retaining named,
plain unsaved and absent predecessors. Its completed journal and synced receipt
remain readable by the previous generation. The live routing and the request's
receipt are then published together; the new door waits for cutover and syncs
stored answers before acknowledging them. Engine imports and lookups identify a
receipt by its canonical parent plus filename, independent of symlink or relative path
spelling and whether the receipt file still exists. Retrying a cycle after a CLI
change or loss of its filesystem receipt returns the original answer without switching
again. Migration retries after publication sync the live document and never
reread the retired inputs.

Admission receipts retain the supplied path keys that previous generations
read. A retry also recognizes equivalent parent paths and durably adds its
supplied spelling as an alias before answering, preserving replay protection
for retained consumers and rollback without changing the admission format.

Legacy files stay for rollback. Before migration, turns and check-config
refuse existing legacy inputs. Read errors, invalid path components and dangling
links in either source paths or the live role document fail closed; only genuine
absence permits defaults.

## Pinned engine plugins

Authorized owners can install, update, and remove pinned Conary
`conary-plugin v5` (`job`) engine plugins through the settings write door
(either `settings` / `proj settings` on the host or `burrow-settings-door` on
GitHub issues):

```
settings plugin list
settings plugin show ENGINE
settings plugin picker
settings plugin verify [ENGINE]
settings plugin install ENGINE nix|release SOURCE VERSION SHA256
settings plugin update ENGINE nix|release SOURCE VERSION SHA256
settings plugin remove ENGINE [--fallback ENGINE]
```

- **Owner authentication:** plugin mutations authenticate the caller by
  immutable GitHub account ID against `burrow.settings.owners` (which inherits
  `burrow.settings.door.authors`), using the same account-ID identity check as
  the issue doors ([front-door.md](front-door.md)). An empty owner list fails
  closed. Host CLI mutations reject any job, sandbox, agent/model, or App-token
  caller environment and authenticate the operator through `GET /user`; jobs
  and models acting alone can never install, update, or remove plugins.
- **Artifact verification and sandboxed conformance:** every plugin is pinned
  to an exact source (`nix` executable inside the Nix store, or `release`
  local/HTTPS artifact), version, and SHA-256 digest. Its hash is verified
  before execution during staging and re-verified on every `proj` turn start
  and `proj check-config`. Before activation, `burrow-settings` runs Conary's
  `cn plugin check` inside an isolated `bubblewrap` sandbox (`--unshare-all`,
  `--clearenv`, read-only plugin mount, no network or host state).
- **Immediate schema choices and picker data:** once conformance passes, the
  plugin's validated `describe` models, efforts, and capabilities are published
  atomically into `proj.enginePlugins` and `proj.engineOffers` (see the
  [settings reference](settings-reference.md) for schema types and storage),
  and immediately appear in `proj settings schema` (role routing enums and
  per-job `$defs.jobChoice`), `proj settings picker` (supplying picker data for
  the dashboard), and `proj run --engine/--model/--effort`.
- **Audit records:** every install, update, and removal appends and fsyncs a
  change-log record naming the authenticated actor, numeric account ID, source,
  version, and SHA-256 (plus prior pin on update and fallback/moved roles on
  removal) before atomic publication.
- **Safe updates and removal fallback:** a failed update (hash mismatch,
  sandboxed conformance failure, or incompatible model withdrawal) leaves the
  prior active installation untouched. Removal requires a valid remaining
  fallback engine (from `burrow.settings.plugins.fallback` or `--fallback`),
  refuses removal without one, and otherwise atomically rewrites every affected
  role route and saved setup to that fallback and reports each move.

