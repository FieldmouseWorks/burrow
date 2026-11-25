# Shared flake runner

`burrow-flake-check gate` and `burrow-flake-check negative` own the shared
workflow's evaluation, build scheduling, checkout snapshots and negative
classification. The workflow's `.github/flake-check/*.sh` files only bootstrap
it. `plan.rs` owns the Nix expression used to plan and evaluate outputs.

The gate and negative runner have no Rust dependencies. It uses the workflow's existing `jq`
for JSON, and invokes git, Nix and, where installed, heavy. Each evaluation
has its own Nix process; evaluations finish and derivations are rooted before
builds start. They run side by side, a core and 2G each, as long as the
job's memory lasts: what one holds isn't known before it runs (`memory.rs`
says what burrow's take), so it watches the room left, starts none while
it's short and, should it run out, stops the biggest but the eldest, to
evaluate again alone once the rest have (#1484). A negative evaluation
failure must repeat in a private local store without substituters, with the comparison tree evaluating afterwards.
That store gets each input that can be fetched, one by one, so an unrelated
host's outage can't decide the PR (#1212); a second try that reaches for an
input it wasn't given can't tell.

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

The behavioral tests live in `burrow-workflows/tests/workflows/gate`: they run the workflow's
own steps with real git and Nix evaluation, including the private-store retry,
base failures, fetches, signals, VM slots, memory limits and test coverage.

## Waiting for kept builds

On a same-repository PR that missed its exact base cache, `run.sh wait`
first runs the planner (`cold`) with its own Nix cache. A PR's exact key is
the merge step's base tip (`steps.merge.outputs.tip`), the commit whose push
saved it, so an exact hit skips the wait. The planner writes
`cold.txt` and returns before builds in either evaluation mode, including
whole-flake fallback paths. A plan it cannot safely produce is left to the
real gate; `COLD_ONLY` no longer controls the gate's build policy.

The API waiter is a separate Cargo binary, `burrow-flake-wait`, using
`burrow-github` for requests, retries, pagination and URL encoding. The wait
bootstrap builds it with the same pinned compiler and lock file, in a fresh
private target directory; wait steps pay this dependency build cost and
registry access. Gate and negative runs retain the direct compilation path.
Only an unfinished push of the same workflow at the merged base's exact SHA
on the default branch justifies waiting, and only while that push's own leg
(the job named as this one, `LEG_JOB`) is running on a runner. One still
queued, or not created yet, may want the very runner this leg holds, so the
leg builds cold rather than wait (#929). A leg on a runner isn't waited for: its deadline
(`WAIT_DEADLINE`, in seconds, from that leg's own start) defaults to 0, so
this leg builds cold now, and restores the base's builds only if they're
already saved. The wait was for codex, whose cold build took 2 to 3 hours;
since it left the machine (#1204), a waiting leg spent 12.3 minutes waiting
to save 2.7 of building (45 PR legs, 2026-11-19). #989 retires the wait
with the kept builds. A failed push is no reason not to wait: its legs keep their
builds whether its gate passed or not (#812), under its own commit, which
is the key this leg restores. A newer push's entry for the leg is: the
base push's leg skips its save once one is listed, and deletes its own
if one turns up after (`burrow_keep::prune`), so the waiter asks that
same question of the leg's entries and, on a yes, stops and restores the
newer entry, which holds every build of the base's that the newer push
didn't change (#1164: a base leg compiling cold for three hours skipped
its save that way, and the PR leg that had waited on it built cold into
the job's six-hour limit). The waiter lists the push's runs once, then
polls only their jobs (`WAIT_POLL`, 60 seconds), and reads the leg's
entries on the first poll, every `WAIT_NEWER` (300 seconds) and once the
leg isn't building, which keeps a long wait inside the token's hourly
requests. A leg that ended without saving, with no newer entry, or no
such run, ends the wait and this leg builds cold. A request
past its poll's deadline (`WAIT_REQUEST`, 120 seconds) fails the step,
which reports `degraded/kept-wait`. A saved entry that then fails to
restore (evicted, or unextractable) leaves the leg building without it and
reports `degraded/kept-rewait` (#1161); what it left half extracted,
Nix's `fallback` builds.

The job inherits the caller's token permissions. Shipped callers grant
`actions: read` for both runs and caches, including in private repositories;
existing callers without it still run the gate and report a soft wait
failure when GitHub denies an API read. A saved cache is restored with its
exact key before the gate runs.

## The build store's shadow

With a build store named (the gate step's `BUILDS` and `BUILDS_KEY`, from
the caller's Actions variables `BURROW_BUILDS_URL` and
`BURROW_BUILDS_PUBLIC_KEY`, on a runner that installs its own Nix), a leg
says before it builds what the store would have served of the derivations
it builds cold (`src/builds.rs`): how many and which, their bytes, how long
fetching them would take (estimated from one NAR's first 64 MiB, timed), and
how long the kept builds' restore took. The last line is
`builds-shadow {JSON}`, for a reader of the job's log. It reads narinfos
concurrently by hash with `curl` and verifies their signatures offline
with `nix store verify --no-contents`, counts only what the store's key
signs, and never fails or holds up the leg: each question has a deadline.
Nothing substitutes from the store.

Such a run's builds go to the store, so every nix the worker starts also
gets `accept-flake-config = false` and `sandbox = true`, after the caller's
config: a flake's own `nixConfig` can't add a substituter or a key, or turn
the sandbox off, whatever the runner's config says. After the gate,
flake-check.yml's builds-write step uploads what the leg built
(crates/burrow-keep, `burrow-keep-upload`).

## A build that stops making progress

The gate and negative runs give every build Nix's `max-silent-time` of an
hour (`SILENT_SECONDS` in `src/main.rs`), after the caller's own
`NIX_CONFIG`: a build that prints nothing for that long is killed and the
leg fails, rather than hold its runner until the run's own limit runs out.
The other builds go on (`--keep-going`), and a push's leg keeps what they
built. The longest quiet stretch in a burrow build that went on to pass was
14 to 16 minutes (codex linking, on a hosted runner).

A run, its evaluations and builds together, has a limit of its own
(`RUN_SECONDS` in `src/main.rs`, which says why that figure): past it
`timeout` stops the worker and everything it started, and the run can't
tell, saying so: a gate signs `error` with why, and a
negative leaves no result to attest. Each job of flake-check.yml has a
`timeout-minutes` of its own: one that runs the gate or the negative gets
its run's limit, the limits of its other steps and half an hour more, which
burrow-workflows' `timeouts` test holds it to (#1488). A job with none
took GitHub's six hours, and one still going holds every failed job of
its run from being run again.

The negative builds each check it judges on its own, once what the check is
built from has built under that hour, with Nix's shorter `timeout`
(`CHECK_SECONDS` in `src/negative.rs`, which says why that figure), and keeps its
log to say why it failed: no check is built a second time. A check that
timed out there, where every build its log started was its own, is the new
tests catching the base when the same check passes on the base and on the
PR's head: a test that a wait ends waits forever on the code with that
bug. Anything else about a timeout can't tell (#1212).

## A check that flakes

With `FLAKY_EXPIRED` and `FLAKY_REPORT` both set and not empty, the gate
keeps the contract in [docs/flaky-tests.md](../../docs/flaky-tests.md#what-a-gate-does)
for its checks (`src/flaky.rs`); unset, it builds as it always did. After
the build fails, a check whose own derivation is the one left to build (its
`nix-store --realise --dry-run` names it alone) is built once more on its
own, in the slot it took, linked apart from its first try, and without the
build store's shadow again. Its name is its attribute under
`checks.SYSTEM` (`browser`). One that passes goes in the report, with the end of its
first try's `nix log`, and counts as a pass; that pass is what the leg
signs, its log holding both tries. A check that fails again, is named in
`FLAKY_EXPIRED`, has a name the flaky tool refuses, or failed because a
derivation it needs did (a build that broke, not a test), fails the gate
as before. The planner (`cold`) and the negative run give no second try.

The workflow's `flaky` input (off by default) runs the flaky action's
`start` step before each leg's gate and its `file` step after, from the
workflow's own commit, so the caller grants `issues: write`. A leg whose
report can't be filed fails, and a pass it signs is then an `error`
("A gate that breaks"). A gate-reuse hit runs neither: it builds nothing.

## A failure the negative tree made

The negative tree is the base's code with the head's version of each test
file the PR changes, deletions included, so it can take away a file the old
code names: a moved test module's, and then nothing that declares it
formats, compiles or evaluates, whatever its tests assert (#1418). A check
that fails there, built or evaluated, is caught only where the new tests
are what fail it (`src/removal.rs`):

- With no test file deleted, or where the removal alone doesn't break the
  check, the failure is the new tests', as judged above. Not compiling for
  want of what the change adds is one. The removal alone is the base with
  those files taken away and, where the PR replaced one, its old content in
  what replaced it. A check only the new tests define has no control
  there: the removal alone breaks it unless its derivation is the same with
  every deleted file put back (a file or directory that took one's place
  gone to make room), which says it reads none of them (#1434).
- Where the removal breaks it by itself, it counts only if it fails, judged
  the same way, with them put back beside the new tests.
- Where it doesn't, the tree made the failure: it can't tell, so nothing is
  signed unless another check catches the base. Where the removal breaks
  the checks' names, each check is judged on the tree with them put back,
  and only a catch is signed.

A file the PR replaced is one that became a directory, or took a Rust
module's or target's other spelling (`x.rs`, `x/mod.rs`, `x/main.rs`). Its
paths don't say whether old code finds the replacement (`mod x;` does,
`#[path]` doesn't) or whether the two clash (they do for `mod x;`), so a
control does: the base with the files at the paths of the tree to judge by,
and only old content in them. The replaced file stays away where that
passes; else it goes back beside its replacement where that does (a
directory's can't). Where neither does, a failure can't be told from the
file's absence or the clash, so it can't tell. A file in a directory the PR
made a file of its name can't go back beside it either. Every tree takes the
deleted files away before it adds any, so that one's in the other's way in
none.

These trees are only to judge by: the one signed is the negative tree.
They're built only for a check that failed, until one is caught, and never
for one that isn't built here (build-skip).

Three limits. A test changed in a file that moved out of the old code's
reach isn't run by it, so it can't be the catch. A replaced file whose old
content doesn't work from its new path (a relative `include_str!`, say)
can't be shown to be found there, so it can't tell. And other copies that
clash side by side (a Go test helper's in one package, two pytest modules
of one name) aren't told apart from a failing test, where the removal
breaks the check as well.

## A gate that breaks

A gate that didn't pass is signed `fail` only when it said that a check, or
the flake, failed. Anything else is `error`, with the reason as the
predicate's `error` (`src/verdict.rs`, #1400): a log or file it couldn't
write, a command killed or unable to run, a tool of its own failing (git,
tar, jq, mktemp, a GC root), a runner that died or never started. It's that
way round because a gate on a full disk can't be counted on to write that
it broke.

- `fail`: a check's build or evaluation failed, the flake doesn't evaluate,
  or the gate refuses the flake or how it was called (the same on a retry).
- The gate leaves `fail`, or `error` and why on one line, in
  `$RUNNER_TEMP/gate-result`. None, or one cut short, is `error`.
- An error's reason is also an annotation on the job, for when nothing on
  the disk could keep it, and is added to the log that's uploaded.
- With legs, the gate job is `fail` if a leg it can hold to its log and
  commit said so, and `error` otherwise.
- What broke is remembered where the gate goes on (the other builds, the
  whole-flake fallback): if it then fails that's `error`, and if it passes
  it's a pass.
- A failure inside Nix or heavy that exits as a failed build does (a builder
  or the daemon out of disk, while the gate could still write) is still
  `fail`: their log says which, and the gate doesn't read their words for it.

The negative has no `error`: one whose log couldn't be written leaves
nothing to attest, as when it can't tell.

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
{"inputs":{"build-skip":["agents"],"eval-apart":true,"flaky":true,"install-nix":true,"keep-builds":true,"legs":4,"runner":"ubuntu-latest"},
 "source":"tree","workflow":{"commit":"<sha>","file":".github/workflows/flake-check.yml","repository":"FieldmouseWorks/burrow","tree":"<tree of that commit>"}}
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

Equal trees are the whole match for the checked source, so with
gate-reuse on, the gate makes its verdict the tree's (`src/source.rs`): it
evaluates and builds, in either mode, a private checkout of the tree whose
HEAD is a commit of that tree alone, made in a git context of its own (no
parents, fixed authors and dates, no inherited config). Everything Nix reads
of the commit (`self.rev`, `shortRev`, `revCount`, `lastModified`) is then
the same for every checkout of the tree, whichever commit it was, shallow
or full, a repository or a worktree, on the run that attests and on the
push that asks alike; a flake reads a `revCount` and `lastModified` of 1.
Relative submodule URLs resolve against what the checkout's would (its
default remote's URL, or with none the checkout itself), and a checkout
with changes it hasn't committed is refused: its tree isn't what it holds.
Its lock file is the tree's too: a lock that needs changing fails the gate.
The kept-build waiter plans from the same source, and the caller's checkout
is never touched. The gate says `"source": "tree"`.

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
