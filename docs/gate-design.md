# The gate's design

The gate is the CI every repo's flake runs: the shared workflow
(`.github/workflows/flake-check.yml`), the runner it starts
(`crates/burrow-flake-check`) and the builds it keeps between runs. It grew
one speed-up at a time and never had a design; this is it: what the gate is,
why it thrashes, the shape it is moving to, where its line with Conary runs,
and the stages that get it there with CI running. The stages and every open
issue's place in them are tracked in
[#985](https://github.com/FieldmouseWorks/burrow/issues/985). Until a stage
lands, the code is what runs and this is where it is going; each stage's PR
brings [where it stands](#where-it-stands) up to date.

## What the gate is

One question, asked of a tree: do its checks pass? Around it:

- **The workflow.** 1,017 lines and six jobs (`lane`, `check` once per leg,
  `gate`, `docs`, `negative`, `commits`), called from every repo's
  `flake.yml` and `flake-retarget.yml`.
- **The runner.** `burrow-flake-check` (about 2,300 lines) plans a flake's
  outputs, evaluates each in a process of its own, builds the checks and
  runs a PR's negative. The workflow compiles it on the runner with bare
  `rustc`, so it has no dependencies and shells out to `jq`.
- **The kept builds.** `burrow-keep` (about 1,500 lines) and
  `burrow-flake-wait`, both built with cargo on the runner, save what a
  default-branch push built into the Actions cache and wait for it.
- **Beside them.** `heavy` (the machine's build slots), `flaky` (one more
  try and a quarantine, [flaky tests](flaky-tests.md)), `flake-gate` (the
  gate step, run locally), and `burrow-workflows` (about 8,100 lines that
  parse the workflow and run its steps, since that is the only way to test
  a policy written in YAML).

Its results have three readers: a person looking at a PR, the landing flow
(the steward today, Conary's flow after), which reads check runs and
attestations, and the machine, which builds what a hosted runner can't (the
`agents` check boots a microVM inside a VM) as a proof.

## How it works today

- **A hosted runner starts with an empty store.** Everything no public
  cache has is built again: the packages burrow patches, and every check,
  changed or not.
- **So a push to the default branch keeps its builds**, one Actions cache
  entry per leg, keyed by workflow, OS, leg and commit. A later run's leg
  restores the newest entry of the same leg, whole, before Nix is
  installed. The negative run restores every leg's.
- **Legs share a gate out by name.** Every leg evaluates the plan, then the
  outputs whose names hash to it, then builds the checks among them. One
  more job checks that the legs agree and attests them together.
- **A PR may skip the gate** when the train will run it (`pr-gate`), or run
  only the checks that read docs when it changes only docs (`docs-paths`).
- **A check a hosted runner can't build is skipped by name** (`build-skip`)
  and built on the machine instead, by the steward, as a proof.
- **On the machine none of this exists.** A runner there shares the
  machine's store, and the steward's proofs say it outright: a path is
  valid in the store only once its build has passed, so a check whose
  output is there passed and isn't run again.

## Why it thrashes

Thirty PRs and twenty-one issues carry `flake-check:` in their titles
since 2026-10-23. Their causes are five, and each is a missing owner.

1. **The memo has the wrong key.** Nix names every build by its derivation,
   and on the machine that name is the whole memo. A hosted run's is keyed
   by where a build ran (leg 2 of 4, of one commit, on one ref). So a build
   is invisible to the other legs (each leg kept its own copy of the
   patched git), to the negative run unless it restores every leg's entry
   (sixteen pasted steps), and to every ref but the one that saved it.
2. **One writer, and it learns last.** Only a default-branch push may save:
   a PR's run takes its workflow from the PR, and GitHub scopes a cache to
   its ref. So the PR that changes an expensive build builds it and throws
   it away, the push builds it again, and every open PR builds it too until
   the push ends. On 2026-11-12 a red default branch kept nothing, dozens
   of legs compiled the patched Codex cold, and the organization's runners
   were full while the fix sat queued (#798). Keeping on red fixed that
   (#812). Waiting for the push's builds came next (#901), which waited an
   hour and then built anyway, and could hold the runner its own producer
   was queued for (#929, #937). Reusing a pass on the default branch means
   that push builds nothing, so it keeps nothing (#966). Each fix was right
   and each made the next one necessary: a dozen issues and PRs on this one
   cache in two weeks.
3. **The cache is full.** burrow's four entries are 10.27 GB of the 10 GB a
   repo gets (2026-11-13), which is one generation of one branch. There is
   no room for a second writer, and an entry is one archive: a leg restores
   all of it or none.
4. **Six ways to skip work, each with a key of its own.** The docs lane
   (changed paths), `pr-gate` (the base and the train), kept builds (leg
   and commit), the wait (a run still going), pass reuse (the tree, #979),
   the negative's changed checks (a derivation against its base's) and
   `build-skip` (a name the caller gives). The bugs are where two meet:
   reuse and kept builds (#966), a red branch and kept builds (#812), the
   order of two steps (#641: the negative built cold for three days and
   said nothing), the docs lane and a retarget or a title edit (#277,
   #322).
5. **The workflow is the program.** Who restores what, under which key, in
   which order is written in YAML expressions: the cache key 39 times, the
   concurrency group six, the attestation three (#728). Three binaries are
   built three ways on the runner. And nobody plans capacity: the legs are
   a fixed number per repo, so a one-line PR takes four runners to
   evaluate, and a lock bump takes four runners to build the same package
   four times.

## The design

Five rules, one for each cause.

### 1. A check is a derivation, and it passed if a trusted run built it

The gate's answer for a tree is the answer for each of its checks, and a
check is a derivation. Its output exists only once its build has passed. So
a check whose output is in a store that only trusted runs write has passed,
and nothing builds it again. That is already the rule on the machine, and
already what `keep-builds` promises ("a check whose derivation hasn't
changed gets the result that push's gate got"). The design makes it the one
rule everywhere, and the one memo: every other way of skipping work either
falls out of it or belongs to the flow that decides whether to gate at all
(rule 5).

### 2. One build store, written by whoever may sign

**The store** is a Nix binary cache the machine serves (`burrow.builds`,
a module like the rest): every hosted run of a public repo substitutes from
it, and its paths are signed with the machine's key. It is keyed the way
Nix keys everything, so a build is one path, held once, whoever needs it.

**Who may write it** is the question the kept builds answered with "the
default branch". The better answer exists already, in Conary's
[rules for evidence](https://github.com/FieldmouseWorks/Conary/blob/main/docs/ci-attestation.md#checking-one):
GitHub signed the job's token, the workflow that asked for it is the signer
workflow at a commit on its default branch, and the job ran on a runner
started fresh for it. A run that meets them can already sign "this tree
passed", and the train lands on that. Saying "this derivation built this
output" is a smaller claim from the same run. So:

- A run uploads what it built with a token whose audience names the upload,
  as an attestation's names its statement. The store's door checks it by
  Conary's rule, through `cn`, never a copy of the rule, and takes only
  repos the machine lists.
- A train candidate, a default-branch push and a PR's run through the
  signer's pinned copy all write. A PR's run of its own copy of the
  workflow (burrow's `check`) reads and doesn't write, exactly as it can't
  sign.
- Nothing of the candidate's runs in that job outside Nix's sandbox, which
  is what the signer rule already depends on. An output's path is its
  derivation's, so a PR can add only the outputs of derivations it defines.
- Where a run substitutes from is never a caller's input: a PR's run takes
  its caller from the PR, and could name another store. The workflow reads
  it from the repo's or organization's Actions variables, and the result
  names it, so a verifier can refuse a pass built from somewhere else.
- A private repo's runs are on the machine, whose store they share already.
  Its paths are never admitted.

Then the PR that changes an expensive build is the run that builds it, once,
hours before anything else needs it. The default branch's push finds it
there, and so does every later PR. Nothing waits for another run, a red
branch changes nothing, and a push that reuses a pass has nothing to keep
warm.

When the store can't be reached a run builds cold and says so
(`degraded/builds`, [degradations](degradations.md)). The store has a size
cap and drops what was fetched longest ago; a dropped path is built again
by the next run that needs it.

Weighed and set aside: a bigger Actions cache (the same scoping, so the
same single writer), a hosted cache or bucket behind a secret (whoever
holds the secret is the writer, which is the default branch again), passing
builds between runs as artifacts (it needs an index and this same trust
argument), and Magic Nix Cache (tried 2026-10-29: no hits).

### 3. One plan a run, facts in and builds out

`burrow-flake-check plan` takes a tree and the store and says, for every
check: it's built already; or it needs building, and what under it is cold;
or this kind of runner can't build it. It is a function of those two
things, so it is tested without GitHub and without YAML.

A run is then the plan, carried out:

- **The plan job** evaluates once. What more than one check needs and no
  store has (a patched package after a lock bump), it builds, once, and
  writes to the store before any leg starts.
- **Legs build and nothing else.** Each takes its share of the plan's
  derivations and evaluates nothing. The plan shares the work out by how
  much each check has cold, across as many runners as it's worth, up to the
  repo's `legs`. A PR that changes one crate gets one leg. A run with
  nothing to build gets none: the plan job says so and ends.
- **No leg builds what another builds, and none waits on another.** What
  they share was built before they started.
- **The join** is the same program: every build the plan asked for has a
  result, on the plan's tree.

A runner on the machine does both halves in one job. It is the same
program, and the same plan.

### 4. The workflow launches, the program decides, and one file says what happened

The workflow keeps what only GitHub can do: jobs, permissions, runners, the
matrix the plan gives it, artifacts, and asking for a token. Every decision
is `burrow-flake-check`'s, one binary, which a run gets from the store like
any other build (so it can have dependencies, and the `rustc` bootstrap and
`jq` go).

A run writes one result: the tree, the gate's identity, and for each check
whether it was there already (and which run built it), built here, failed,
flaked or left to another kind of runner, how long it took and what was
cold under it. Everything that tells anyone what happened is that file
printed: the step summary, the predicate Conary signs, the flaky report,
what the watchdog reads for drift (#772), and `flake-gate` on a desk.
"Why did this leg take two hours" is a line in it.

### 5. Conary owns evidence and when, burrow owns builds and how

This is the line M7 needs, since M7 cuts Conary's ties to burrow.

| | Conary's, in `cn` at a commit the workflow pins | burrow's, in `burrow-flake-check` |
| --- | --- | --- |
| What a result is | subjects, predicates, signing (`cn attest`), the verdict words | the result file `cn attest` reads |
| A negative | which files are tests, whether a PR needs one, the negative tree | which checks that tree changes, building them, whether the failure was the test's |
| Trust | the verifier and the signer rule | asking it: for evidence, and for the store's writers |
| The memo | a pass by tree (#979, Conary#291) | a build by derivation (rule 2) |
| When | whether a tree is gated at all: the train, deferral (Conary#296), batches (Conary#320) | making a gate cheap when it runs |

One file crosses the line in each direction: the result goes in, the
attestation comes out. burrow formats no subject and no predicate (#728),
and keeps no list of test patterns: Conary's planner has the one copy.

Whether `flake-check.yml` stays the signer workflow, or repos call Conary's
`attest.yml` with the gate's jobs as its `needs`, is M7's migration and
Conary's to make. After stage 3 it changes one call here. Two things that
migration has to settle are known now and aren't this design's: burrow
checks a PR merged with its base as the job finds it, where `attest.yml`
tests the event's commit, and a gate with legs has several logs.

A proof is the same line seen from the machine. A check a hosted runner
can't build is one the plan leaves to another kind of runner. The machine
builds it with this program, on the same tree, and its verdict is the same
fact: the output is in the store. Who starts that build is the flow's (the
steward's proofs today, a Conary proof plugin after,
[Conary#311](https://github.com/FieldmouseWorks/Conary/issues/311)). Which
checks need it stops being a name the caller gives (`build-skip`): a check
says what it needs, and a runner says what it has.

## What stays

- Evaluating each output apart, within the runner's memory, on one snapshot
  of the checkout.
- A PR checked merged with its base as the job finds it.
- The negative's own judgment: which checks a tree changes, and whether a
  failure was the test's.
- `heavy`'s slots on the machine, and the flaky retry and quarantine.
- `pr-gate` and pass reuse by tree: both are the flow deciding whether to
  gate, not the gate saving work.
- Degradations as steps a machine can read, the `commits` job, and a PR's
  new run cancelling its last.

## What goes

| Goes | Why it can |
| --- | --- |
| The kept builds in the Actions cache: their keys, the restore steps (two a leg, sixteen in the negative), the lookup that tells a miss from a failed restore, keeping on red | rule 2: one store, substituted from like any cache |
| The wait: `burrow-flake-wait`, the `cold` planner, the second restore | rule 2: the run that changes a build wrote it |
| Keeping the default branch warm after a reused pass (#966) | rule 2: nobody needs that push to build |
| Legs by a name's hash, every leg's own evaluation, the join in shell | rule 3: one plan, shared out by cost |
| The `rustc` bootstrap, `compiler.nix`, `jq` as the codec, cargo builds on the runner | rule 4: the runner is a build in the store |
| The attestation's three copies, and the test patterns' second copy | rule 5: Conary's |
| The docs lane: `docs-paths`, `docs-checks`, the `docs` job, the check-run names the flow parses | rule 1, once a docs-only PR's plan has nothing to build |
| `build-skip` | rule 5: a check says what it needs |

By the files they live in that is about half the workflow, `burrow-keep`'s
archive half, `burrow-flake-wait`, and the tests that hold them up in
`burrow-workflows`. The count is an estimate until the stages land.

## The stages

Each stage is one task, deletes what it replaces, and leaves CI running in
every repo: a caller pinned to an older workflow keeps working until its
pin moves.

| | Stage | Lands | Deletes | Issue |
| --- | --- | --- | --- | --- |
| 0 | What's in flight | the wait fixed (#937), pass reuse behind its flag (#979) | | #937, #979 |
| 1 | The build store | `burrow.builds` and its door; every run that may write does; then runs substitute from it | the kept builds, the wait | [#986](https://github.com/FieldmouseWorks/burrow/issues/986), [#988](https://github.com/FieldmouseWorks/burrow/issues/988), [#989](https://github.com/FieldmouseWorks/burrow/issues/989) |
| 2 | One plan | `plan`, the plan job, legs that only build, the join and the result file in the program, the runner from the store | name-hash legs, the bootstrap | [#990](https://github.com/FieldmouseWorks/burrow/issues/990) |
| 3 | Signed by Conary | `cn attest` reads the result file; the negative's tree and patterns come from `cn` | the attestation's copies, the second pattern list | #739, #740 |
| 4 | The docs lane retires | a docs-only PR runs the gate, which has nothing to build | the lane | filed when 2 lands |
| 5 | Checks say what they need | the plan leaves a check to the runner that can build it; the proof calls this program | `build-skip` | filed after the steward is gone |

The rules the stages keep:

- **One at a time.** They are the same file.
- **Anything a run trusts runs in shadow first.** Stage 1 writes the store
  from every run that may, and each leg reports what the store would have
  served beside what it built, before any run substitutes from it. The
  switch is made on what those reports show (hits no worse than the kept
  builds', fetch time no worse than their restore), the way Conary's flow
  shadows the steward.
- **No switch in the days conaryd takes over a repo.** Stage 1's store and
  its shadow change nothing a run decides, so they can land any time. The
  switch, and stage 2, wait until that repo's cutover has settled: two
  engines swapped in one week is how you can't tell which one broke it.
- **Stage 4 is a measurement, not a promise.** The lane goes only when a
  docs-only PR's plan shows nothing to build in every repo that sets
  `docs-paths`. Where one rebuilds, the fix is that check's source, and the
  lane stays until it's made.
- **Each stage's tests fail with its change reverted**, and from stage 2
  one test is of the design itself: plan a tree twice against the store its
  first run wrote, and the second plan builds nothing.

### Where it stands

Stage 0 is in flight. Nothing else has landed.

## What this doesn't do

- It doesn't change what a check is or which checks a repo has: the format
  checks (#664), the negative's `testFiles` (#699) and the flaky tests are
  the gate's content, and stay where they are.
- It doesn't decide when a tree is gated. Fewer gate runs per landing is
  the flow's work (Conary#291, #296, #320); this makes each one cost what
  changed.
- It adds no timer and no polling: a run reads the store when it plans.
- It doesn't serve the machine's own store. Only paths the door admitted
  are in the build store.
