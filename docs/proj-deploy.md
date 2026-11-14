# Usage records and turns across a deploy

The shared usage record (`proj.usageHold`'s file, named in the
[settings reference](settings-reference.md)) and immutable `.usage-owner-*` metadata retain the
strict pre-trickle `Spent` schema. Readers accept the short-lived generation
that added `admission`, but writers never serialize that field.

A release first stages its nanosecond cutoff in `.usage.admission`. The
compatible record's `job` reference encodes the precise cutoff and commits it
together with `back`; a staged cutoff cannot release an active hold. Acknowledgement and
settlement retain the cutoff before removing the record. The journal keeps
its tab-separated cutoff for earlier trickle readers. An old writer preserves
the reference when updating the released record. If an old generation deletes
the record after an interrupted acknowledgement, recovery conservatively
includes the original natural-reset cohort. A release written entirely by an
old binary includes the whole `back` second: filesystem timestamps may be
coarser than the new queue timestamps, so using the record's modification
time could omit same-second waiters. New releases retain exact nanoseconds.

`proj-retry-barrier` holds the original `.slots/.lock` for the deployment's
lifetime. It stays running across package changes, so restarting the transfer
consumer never reopens admission to retained pre-trickle binaries. Current
schedulers serialize on `.slots/.admission-lock`; they still census the same
physical slot files, including existing holders. Emergency and Codex pools
have their own slot files and do not pass through the barrier.

`proj-retry-transfer` scans at startup and watches job and queue events. A
waiting old turn is checkpointed before its transient unit stops, then starts
again as the same turn on the deployed executable. The checkpoint preserves
the queue name and arrival time, original deadline, refusal prompt and engine.
The conversation and stream stay in place. End hooks and watchers leave the
checkpoint alone, and a failed transfer resumes from it. A later refusal by an
old running turn triggers the same transfer. New retries write the checkpoint
before waiting and allow ordinary in-process executable handover, so later
deployments do not need another census fence. A retained turn only hands
over to a binary advertising the checkpoint format; on rollback to an older
binary it keeps its current executable and conversation.

The process regressions in `crates/burrow-proj/src/retry/tests.rs` run the
pre-trickle scheduler snapshot alongside the current one, including an
unfenced control that starts five turns at a reset. Usage tests exercise the
real strict legacy schema, owner generations, interrupted writes and cleanup.

Pending checkpoints remain queued work in lifecycle and watchdog snapshots,
even while their unit is inactive or its startup failed. Their saved queue
places remain visible to the census until admission or cancellation, so dead
parent PIDs and failed launches cannot make the reset cohort disappear.
Cancellation, checkpoint consumption and recovery launch decisions share the
job lock. Failed cancellation retains the checkpoint; successful cancellation
removes it. Only a startup explicitly marked `retry-startup-failed` can retry
an outcome: a terminal outcome from an unaware old binary never replays.
Lock contention gets an explicit delayed retry within the consumer, without
watching its own lock-file events or failing the deployment service.
Initial transfers start their configured budget at acquisition; retained
retries restore their remaining budget, then publish the final watchdog state.

Recovery authority, startup-failure markers, launch rate limits and the saved
execution identity live under the sandbox-denied host control root, rather
than the job directory. Guest-written checkpoint-shaped files are ignored.
New starters save engine, model, effort and execution metadata before the
engine runs. Legacy parents without that identity are adopted only when
systemd identifies the main process and the kernel proves it is blocked on
the fenced legacy census lock. Sleeping legacy retries remain fenced until
that proof is available at reset. Recovery restores the saved execution
identity; mutable job files cannot broaden its capabilities.

What a job runs as on every turn and every engine run (its project, pinned
engine, role, GitHub App, sandbox and `--owned`) is its request, which the
starter saves in the same host control root (`crates/burrow-proj/src/job/request.rs`).
Only the starter and the inbox's label-to-role enrollment change it. A role is
scoped to the repository its admission bound, not to its project's current
origin. The job directory's files of those names are a published copy, put
back at each turn's start. A job from before the request adopts it once, from
a retained retry's captured files if it has them (what the starter fixed from the
earliest, engine and role from the latest), else from its published files. A
retry's execution record still carries those files in the earlier format, so a
rolled-back proj's transfer consumer can restore it.

How a job is scheduled and bounded (`--unstick`, `--pipeline`, its turns'
`--timeout` and `--idle`, what each turn's start and end kick, and the
`--link` its GitHub quota is held to) is its schedule, saved beside the request
(`crates/burrow-proj/src/job/schedule.rs`). Only the starter and `proj unstick`
change it. The line's class, project cap and emergency slot, the retry census,
the inbox drain's limits, each turn's budget and quiet limit, its kicks and its
quota hold all read the schedule, with the project from the request; the job
directory's files of those names are a published copy, put back at each turn's
start. A job from before the schedule adopts its published files once.

Each retained turn occupies one queue place, even after promotion or a
configuration-driven class change. The census and replacement adoption remove
obsolete names under the admission lock. Every replacement launch records its
next retry delay first, including startups unable to write an outcome; delayed
jobs do not hold up healthy jobs or independent pools. Confirmed cancellation
finishes deferred end processing once, irrespective of end-hook ordering.

A saved execution identity does not prove that a parent is waiting. Transfer
requires either its pending host-owned checkpoint or the kernel census-wait
proof, regardless of engine or pool. Deleting guest slot/outcome files cannot
authorize stopping active work. Startup saves the exact engine/model/effort
choice it resolved and executes that same value; changing settings between
resolution and persistence cannot change the saved identity.

Recovery writes into job directories use pinned, symlink-free directory
handles. Record replacement creates a fresh temporary file and renames it
relative to that handle, without opening a guest-chosen destination or staging
link. Transcript appends and timestamp updates pin regular files and reject
aliases. Engine prompt input is opened through the same confined reader before
launch. A concurrent pathname replacement cannot redirect these operations to
another host directory. Host-owned records outside guest storage retain their
separate compatibility protocol.

Repository admission also applies to retry recovery. A paused retry retains its
checkpoint, execution identity, conversation and original queue position, but
holds no place in the admission census. Recovery leaves parked work to the
policy reconciler: on release it reacquires drain capacity in queue order and
launches the saved turn with its captured execution identity. It does not make
a new turn or consume the checkpoint while paused.
