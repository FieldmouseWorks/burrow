# Shared flake runner

`burrow-flake-check gate` and `burrow-flake-check negative` own the shared
workflow's evaluation, build scheduling, checkout snapshots and negative
classification. The workflow's `.github/flake-check/*.sh` files only bootstrap
it. `plan.rs` owns the Nix expression used to plan and evaluate outputs.

The runner has no Rust dependencies. It uses the workflow's existing `jq`
for JSON, and invokes git, Nix and, where installed, heavy. Each evaluation
has its own Nix process; evaluations finish and derivations are rooted before
builds start. A negative evaluation failure must repeat in a private local
store without substituters, with the comparison tree evaluating afterwards.

## Delivery and caller cost

The workflow checks out its own immutable `job.workflow_sha`, including this
crate, then compiles it directly with `rustc --edition=2021` in a private
temporary directory, so Rustup cannot read the caller's toolchain file. No Cargo build,
registry access, release download or caller-writable executable cache sits
between those sources and the runner. The local `flake-gate` fetches the same
sources beside the workflow; its Nix package supplies rustc and a C linker.
Self-hosted CI runners need both on PATH, as hosted Ubuntu runners already
have them.

The earlier 16–20 second hosted-runner Cargo build was a poor trade for a gate
bootstrap. This runner instead builds unoptimized, with the standard library
alone. Three fresh direct builds on the development machine took 1.161,
0.792 and 0.794 seconds (the resulting binary was 5,573,448 bytes). These are
local measurements, not hosted-runner measurements. The cost is paid once
per invocation, including paths-only; correctness and a source-bound executable
are worth that smaller cost. CI's step timing will show the hosted cost.
No binary is reused between invocations: a preceding caller step must not be
able to supply the executable that judges its tests.

The behavioral tests live in `burrow-workflows/src/gate`: they run the workflow's
own steps with real git and Nix evaluation, including the private-store retry,
base failures, fetches, signals, VM slots, memory limits and test coverage.
