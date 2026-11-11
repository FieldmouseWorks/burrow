# Usage records and turns across a deploy

The shared `.usage.json` and immutable `.usage-owner-*` metadata retain the
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
