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

## Reusing an attested pass

With the workflow's `gate-reuse` input on (it's off by default, and no
caller burrow ships turns it on), a push to the default branch asks the
caller's `gate-reuse-lookup` whether CI already attested a pass of the same
gate on the same tree, before any job builds. `burrow-flake-check reuse`
(`src/reuse.rs`) asks and reads the answer; the workflow's `identity` step,
one step shared by every job that needs it, says which gate a run is.

**The gate** is `$RUNNER_TEMP/gate.json`, one line of JSON with its keys
sorted:

```json
{"inputs":{"build-skip":["agents"],"eval-apart":true,"install-nix":true,"keep-builds":true,"legs":4,"runner":"ubuntu-latest"},
 "workflow":{"commit":"<sha>","file":".github/workflows/flake-check.yml","repository":"FieldmouseWorks/burrow","tree":"<tree of that commit>"}}
```

`workflow` is this workflow (`job.workflow_*`), `inputs` its inputs that
can change a result, `build-skip` as a sorted set. With `gate-reuse` on,
every result the workflow attests carries it as the predicate's `gate`,
after the fields it always had, so a later push can be matched to it; off,
the predicate is unchanged. Conary's verifier ignores fields it doesn't
know. burrow#728 moves attestation production into Conary's `cn attest`,
which then takes this field as given.

**The request** is an HTTP POST of one line of JSON, keys sorted:

```json
{"commit":"<the push>","contract":"burrow-gate-reuse/v1","gate":{...},"repository":"owner/repo","tree":"<the push's tree>"}
```

Its query digest is the SHA-256 of those exact bytes, in lowercase hex.
It carries `Authorization: Bearer <token>`: the job's GitHub OIDC token,
whose audience is `burrow-gate-reuse:v1:sha256:<query digest>`, so the
lookup can tell which repository and workflow ask, and that they asked this;
a job that can't get one asks nothing. The URL is https, or http on the
runner's own machine (127.0.0.1, localhost or [::1]), its authority a bare
host and port, with no user information. Only a push to the default branch
by its exact name asks; any other ref builds, as without gate-reuse.

**The answer** is HTTP 200 with one JSON object:

```json
{"contract":"burrow-gate-reuse/v1","query":"<query digest>","evidence":"hit","run":"https://github.com/owner/repo/actions/runs/123"}
```

`evidence` is `hit`, `miss` or `unknown`. It is a hit only when the lookup
holds CI's own attestation, verified as Conary's train verifies evidence,
of a passing gate on exactly `tree` in `repository`, whose `gate` is the
request's in everything but `workflow.commit`: two commits of the workflow
with the same tree are the same files, so `workflow.tree` stands for the
commit. burrow's own main push runs the workflow at the pushed commit, which
no earlier run used, so only the tree can match it. `run` names the run
whose evidence it is (`/attempts/N` or `/job/N` may follow).

Equal trees are the whole match for the checked source, as Conary's rule
that evidence is about the tree it ran on says (its DESIGN, section 4.3,
rule 3): a landing is always a new commit, so nothing could be reused
otherwise. A check that reads the source's commit metadata (the flake's
`self.rev`, `revCount` or `lastModified`) saw the commit the pass was
attested on, not the push's.

Only a hit, read whole, for this request (its `query`), naming a run of this
repository on this server, skips the gate: the job named check (the gate
job, with legs) reports the pass, linking that run, and builds, attests and
keeps nothing. A miss runs the whole gate. Anything else (no answer, another
status, `unknown`, an answer that isn't one JSON object, another contract or
query, a run that isn't this repository's, a push the event names no tree
for, a token the job can't get) runs the whole gate too, and the lookup step
fails softly (`degraded/gate-reuse`).

A push that reuses a pass keeps no builds: it built none. The Actions cache
lets a run restore only entries saved on its own ref and the default
branch, so a PR's or a train candidate's builds can't warm the default
branch or later PRs, and the default branch's entries already fill the
repository's cache (burrow's four were 10.3 GB on 2026-11-13), so saving more
would evict the ones every PR restores. Only a push to the default branch
that builds keeps its builds, as without gate-reuse.

Nothing on a machine runs this, so it has no NixOS option: a caller turns
it on with the workflow's two inputs.
