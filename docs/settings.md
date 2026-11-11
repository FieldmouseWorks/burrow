# Automated work settings

`burrow.settings.allowed` is the tracked allow list, set by the importing
machine's flake. It defaults to empty: new repos cannot drain issues or
start automated inbox, steward fix, judge or proof jobs until opted in.
Triage and its planner sessions can sort issues in unlisted repositories;
offering planned children and implementation stay gated. An explicit runtime
pause or scoped exclusion parks every role, including sorting and manual work.

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

A pause vacates running turns and proofs through an independent settings
reconciler. It kills their complete service groups, releasing job, Codex,
proof and inherited heavy locks. Queued turns, questions and awaits are
parked without holding admission capacity or producing stall alerts.
Job scratch, worktrees, native sessions, conversations and proof checkpoints
remain. On release, queued turns return at their original order and priority;
interrupted turns continue their session, and proofs retain completed checks.
Question and await deadlines advance by the parked duration. The overlay path
wakes reconciliation immediately, scope expiry books its own wake, and a
short recovery timer handles a machine restart or interrupted reconciliation.

Automation binds its repository identity in host-owned control state before
launch. A missing identity fails closed; deliberately local automated work
uses `proj run --local-work`. A worker cannot evade policy by changing its
project or role metadata, or by making its Git remote unreadable.
