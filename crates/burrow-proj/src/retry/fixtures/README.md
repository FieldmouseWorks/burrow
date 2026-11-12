These are the production scheduler and picker from 3e57596cea8 (the parent of 3bdabc2), used as retained retry processes
in the deployment regression. Only the test-module declarations are removed,
and slot.rs's picker import is redirected to the accompanying snapshot.
They intentionally keep `take(false)` and the original `.slots/.lock`.

`slot_nonblocking.rs` and `pick_nonblocking.rs` are the same from 92a36dc11697,
the generation that polls `.slots/.lock` with `try_lock()` and so never shows as
a blocked lock in `/proc/locks`.
