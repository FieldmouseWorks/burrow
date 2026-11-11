These are the production scheduler and picker from 3e57596cea8 (the parent of 3bdabc2), used as retained retry processes
in the deployment regression. Only the test-module declarations are removed,
and slot.rs's picker import is redirected to the accompanying snapshot.
They intentionally keep `take(false)` and the original `.slots/.lock`.
