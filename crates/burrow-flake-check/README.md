# Shared flake runner

`burrow-flake-check gate` and `burrow-flake-check negative` own the shared
workflow's evaluation, build scheduling, checkout snapshots and negative
classification. The workflow's `.github/flake-check/*.sh` files only bootstrap
it. `plan.rs` owns the Nix expression used to plan and evaluate outputs.

The gate and negative runner have no Rust dependencies. It uses the workflow's existing `jq`
for JSON, and invokes git, Nix and, where installed, heavy. Each evaluation
has its own Nix process; evaluations finish and derivations are rooted before
builds start. A negative evaluation failure must repeat in a private local
store without substituters, with the comparison tree evaluating afterwards.

## Delivery and caller cost

The workflow checks out its own immutable `job.workflow_sha`, including this
crate, its lock file and the crate toolchain specification. `compiler.nix`
gets Nixpkgs and rust-overlay from that lock, and the minimal Rust channel
from `crates/rust-toolchain.toml`. Nix supplies both the compiler and C linker;
no runner needs a Rustup default, a Rust installation or an ambient linker.
This also covers burrow's fresh runners without changing the machine flake.
The local `flake-gate` fetches the same files at the named workflow commit.

The compiler closure stays rooted while the bootstrap compiles the runner
in a private directory. No Cargo dependency build, registry access, release
download or caller-writable executable cache sits between the pinned sources
and the runner. Caller toolchain files cannot choose its compiler. Both gate
and paths-only runs require Nix: the negative job now installs it before its
path filter. Kept-build restores remain conditional on needing a negative;
compiler provisioning does not query that cache before it has been restored.

The earlier 16–20 second hosted Cargo build remains avoided for gate and negative runs. The bootstrap
reports toolchain provisioning and direct compilation separately. A real run
with broken ambient `rustc` and `cc` commands took 1 second for provisioning
and 2 seconds for compilation with the compiler already in the local store.
The earlier [hosted CI run](https://github.com/FieldmouseWorks/burrow/actions/runs/42508563466)
reported 1 second per gate compilation using the host toolchain; that excludes
the new Nix provisioning and is not an end-to-end timing of this bootstrap.

A cold store must fetch compiler inputs and their closure (about 1.1 GiB
uncompressed on x86_64 Linux). The first completed hosted leg of the
[new bootstrap run](https://github.com/FieldmouseWorks/burrow/actions/runs/42517486906/job/128843615710)
reported 720.6 MiB of compiler fetches, 11 seconds of provisioning and
1 second of compilation. This is one observed cold run, not a timing bound.
Using the minimal pinned toolchain reduces that from about 1.9 GiB with
Nixpkgs' general rustc package. The store shares those inputs with Rust gates
and subsequent runs, but a non-Rust caller still pays that cold setup cost.
We accept it to supply a working, pinned compiler on every supported runner
without a separate binary publication and verification system. Paths-only
runs now pay Nix setup as well. This is a cold provisioning cost, not just
the one-second compilation figure; CI logs expose both costs explicitly.
No runner executable is reused between invocations.

The behavioral tests live in `burrow-workflows/src/gate`: they run the workflow's
own steps with real git and Nix evaluation, including the private-store retry,
base failures, fetches, signals, VM slots, memory limits and test coverage.

## Waiting for kept builds

On a same-repository PR that missed its exact base cache, `run.sh wait`
first runs the planner (`cold`) with its own Nix cache. The planner writes
`cold.txt` and returns before builds in either evaluation mode, including
whole-flake fallback paths. A plan it cannot safely produce is left to the
real gate; `COLD_ONLY` no longer controls the gate's build policy.

The API waiter is a separate Cargo binary, `burrow-flake-wait`, using
`burrow-github` for requests, retries, pagination and URL encoding. The wait
bootstrap builds it with the same pinned compiler and lock file, in a fresh
private target directory; wait steps pay this dependency build cost and
registry access. Gate and negative runs retain the direct compilation path.
Only an unfinished push of the same workflow at the merged base's exact SHA
on the default branch justifies waiting. Every poll reads status before the
exact cache lookup, including after a failed producer run or completion
during planning. No matching run and no saved cache ends the wait promptly;
a running producer is bounded by `WAIT_MAX` (3600 seconds by default).

The job inherits the caller's token permissions. Shipped callers grant
`actions: read` for both runs and caches, including in private repositories;
existing callers without it still run the gate and report a soft wait
failure when GitHub denies an API read. A saved cache is restored with its
exact key before the gate runs.
