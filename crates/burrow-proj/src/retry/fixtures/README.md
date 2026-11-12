These are the production scheduler and picker from 3e57596cea8 (the parent of 3bdabc2), used as retained retry processes
in the deployment regression. Only the test-module declarations are removed,
and slot.rs's picker import is redirected to the accompanying snapshot.
They intentionally keep `take(false)` and the original `.slots/.lock`.

`nonblocking/` holds the same two files from 3bdabc2, with the same edits.
Its `take` polls `.slots/.lock` with `try_lock` every two seconds instead of
blocking on it, which is how the retries of a proj from 3bdabc2 wait.
