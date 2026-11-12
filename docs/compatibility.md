# Persisted-format compatibility

Rollback runs the previous system against state the new one wrote, and an
upgrade runs the new one against state the old one left. A persisted format is
safe only if both directions read. [`burrow-compat`](../crates/burrow-compat/src/lib.rs)
is the harness that proves it; this page is its guide and the inventory of what
burrow persists. Parent: [#664](https://github.com/FieldmouseWorks/burrow/issues/664).
It does not replace atomic writes ([#730](https://github.com/FieldmouseWorks/burrow/issues/730))
or locking ([#733](https://github.com/FieldmouseWorks/burrow/issues/733)): those
keep one write whole and one writer at a time; this keeps a whole record readable
across versions.

## Registering a format

A `Format<V>` has a stable id (never renamed: it is the name diagnostics and
this inventory use) and numbered versions. Each version brings:

- `read` and `write`: the production adapters for that version's bytes. Use the
  code the crate really runs (its serde types, its line parser), not a copy, so
  the harness exercises what ships. Any encoding works; `V` is the value in
  memory.
- `samples`: representative values that version can hold, covering every
  optional field set and unset.

and each neighbouring pair brings a `migrate(from, up, down)`: `up` carries a
value forward, `down` carries it back for a rollback, and a `down` that must
drop something says so in code (or refuses, naming the field).

`Format::check` writes every sample with its own version's writer and reads it
with every version's reader: same version, forward (old data, new reader) and
backward (new data, old reader, the rollback). The read must succeed and equal
the sample carried to that version by the migrations. A `Failure` names the
format, direction, both versions, the stage (write, read, migration,
comparison) and the field when the adapter knows it (`AdapterError::json` lifts
it out of serde's `unknown field`, `missing field` and `duplicate field`).
A crate puts `format().assert_compatible()` in its tests; a `Registry` refuses
two formats with one id.

Example failure, a `deny_unknown_fields` v1 reader meeting v2's `admission`:

```
format `settings-overlay` backward (new data, old reader): v2 written, v1 read, read failed at field `admission`: unknown field `admission`, expected `repos`
```

## Adopting unversioned records

Deployed records carry no version today. Adoption never edits what is on disk:

1. Register the shape the deployed readers accept as version 1. `declared_version`
   reports `None` for a record with no version key, and that reads as version 1.
2. A later version adds its fields as optional and, if it wants a version key,
   adds that last. The check fails it while any registered reader is strict
   (`deny_unknown_fields`), naming the key, so the order is: ship readers that
   tolerate the key, then the writer that emits it.
3. A reader that cannot be made tolerant keeps the writer from emitting the new
   shape until no rollback target reads the old one.

## Inventory

Found by reading each crate's persisted file names and serde types (not every
consumer was traced; a blank consumer is not a claim of none). **Owned** state
is what burrow writes and later reads and is the harness's job. **Generated**
configuration is written by a Nix module (`builtins.toJSON`) from flake options
and read by one program; it changes with a deploy, so the flake and the
package move together and only a rollback of both can mismatch (checked by
the example machine build, not by this harness). **External** formats belong to
another tool; burrow only adapts to them.

None are registered yet: this change lands the harness. Registering a row is
a follow-up per crate, owned by the crate named.

### Owned state (to register)

| Record | Owner (writes) | Consumers | Encoding |
|---|---|---|---|
| settings `overlay.json` (repos, pipelineUntil, receipts; `deny_unknown_fields`) | burrow-settings | burrow-settings, burrow-proj | JSON |
| dispatch `live.json`, `status.json`, `watch.json`, `state.json` | burrow-dispatch | burrow-probe (live), burrow-update (live, status), burrow-units (watch) | JSON |
| `wake.json` (deferred wake-ups) | burrow-github, burrow-events, burrow-renovate, burrow-proj | burrow-events, burrow-watchdog, burrow-usage-reset, burrow-dispatch | JSON |
| watchdog `state.json` | burrow-watchdog | burrow-watchdog | JSON |
| units `cleared.json` | burrow-units | burrow-units | JSON |
| advisory-watch `pending.json` | advisory-watch | advisory-watch | JSON |
| backup `last.json`, `last` | burrow-backup | burrow-backup (status), burrow-probe | JSON; `last` is one plain text line |
| usage-reset `state.json`, meters `ENGINE.json` and `log` | burrow-usage-reset | burrow-usage-reset, burrow-proj (role overflow) | JSON (`deny_unknown_fields`); `log` appended lines |
| proj `.usage.json` | burrow-proj | burrow-proj, burrow-settings (read door) | JSON |
| proj inbox plans `tasks.json` | burrow-proj | burrow-proj | JSON |
| proj roles and models files, `roles.d/` setups | burrow-proj (operator edits) | burrow-proj, burrow-settings | line text (`ROLE ENGINE ...`) |
| upkeep `app-hook.json`, `answers.json`, per-repo state | burrow-upkeep | burrow-upkeep | JSON |
| dashboard `state.json`, `enroll.json` | burrow-dashboard | burrow-dashboard | JSON |
| update `manifest.json`, `status.json` | burrow-update | burrow-update | JSON |
| flaky `report.jsonl` | flaky | flaky | JSON lines |
| proof `build.json`, `ran.json`, `end.json`, `policy-env.json` | burrow-proof, burrow-proj | burrow-proj | JSON |
| flake-check `plan.json`, `entries.json`, `negative.json` | burrow-flake-check | burrow-flake-check | JSON |
| keep store | burrow-keep | burrow-keep | its own text lines |
| lock files (`pass.lock`, `.usage.lock`, `.waits.lock`, `.policy.lock`, `roles.lock`, `sweep.lock`) | burrow-dispatch, burrow-proj, burrow-renovate | the same | empty; the lock is the content (see #733) |

### Generated configuration

`config.json` of advisory-watch, burrow-review, burrow-update and house-sync;
`boot.json` (burrow-boot), `switch.json` (burrow-switch), `firmware.json`
(burrow-firmware), `notify.json` (burrow-notify), the tracked settings file
(burrow-settings `Config`), the handler config of burrow-events. Written by the
matching module in `modules/`.

### External formats

Claude and Codex state (`.claude.json`, `settings.json` of a sandbox, `auth.json`,
the codex `answer.json` and `schema.json`, job transcripts `N.jsonl`), `flake.lock`
and `Cargo.lock`, restic's JSON summary, GitHub payloads. Pin by reading
tolerantly; do not register as owned versions.

## Conary

Conary's persisted formats are not touched here. To adopt the harness a Conary
crate depends on `burrow-compat` (the crate has only `serde_json` as a
dependency), builds a `Format` per record from its real reader and writer, and
asserts it in its own tests. It is bound to Conary's release order: ship readers
that tolerate a field before writers that emit it.
