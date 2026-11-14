# proj's design

proj runs the machine's agent jobs and turns GitHub issues into them. It grew
one feature at a time and never had a design; this is it: what proj is, why
it stalls, the shape it is moving to, and the stages that get it there
without stopping the pipeline. The stages and every open issue's place in
them are tracked in
[#974](https://github.com/FieldmouseWorks/burrow/issues/974). Until a stage
lands, the code is what runs and this is where it is going; each stage's PR
brings [where it stands](#where-it-stands) up to date.

## What proj is

Two programs in one crate (`crates/burrow-proj`, about 53,000 lines, half of
them tests):

- **The job runner.** A job is a conversation with an agent in a sandbox,
  one turn at a time. The runner decides when a turn may start (slots, the
  line, usage and GitHub quota, pauses), which engine runs it, and what
  happens when it ends (it asked the owner something, it awaits a PR, it is
  done, it broke).
- **The inbox.** It reads a repo's labelled issues and makes jobs of them:
  enrols an issue, has a job sort it, plans and splits the big ones, hands
  the ready ones on as there is room, and posts the answers back.

The inbox is the runner's biggest customer, and so are the steward's review
and fix jobs, the watchdog's, and anyone typing `proj run`. Once
[Conary's flow](https://github.com/FieldmouseWorks/conary/blob/main/docs/flow.md)
replaces the steward, its plugins take the steward's place as a customer.

## How it works today

- **A job's state is files.** A job's directory holds some forty kinds of
  marker (which turn, its slot, its outcome, that it ended, that it was
  stopped, its deadline, what it awaits, why it is parked, its question, its
  retry), and the rest of the truth is in systemd's unit states, held file
  locks and `/proc`.
- **Every reader works the state out for itself.** `Job::state`, `holds`,
  `lifecycle` and `facts` each read the markers their own way; so do the
  snapshot, the inbox's view, the inbox's pass (which also changes what it
  reads) and, outside proj, the steward and burrow-upkeep, which parse
  `proj lifecycle`'s lines.
- **There is no scheduler.** A turn that has no slot yet is a process that
  wakes every couple of seconds, takes a lock, counts everyone and decides
  whether it is next. The line is those processes, so a deploy has to carry
  each of them into the new binary ([the deployment protocol](proj-deploy.md)).
- **Waking is messages.** A turn's end kicks the watcher, the steward, the
  inbox and the repos the job awaited. The watcher, the inbox and the policy
  each keep a wake file of their own, and several side lists say who is
  waiting for what. A kick that is lost, or a pass that fails, is made up
  for by a retry some minutes later or by a deadline.
- **The machine's own bookkeeping sits inside GitHub passes.** The inbox
  frees a finished sorting job's slot and records its verdict partway
  through a pass that begins by listing the repo's issues.

## Why it stalls

The stalls of 2026-11-13
([#967](https://github.com/FieldmouseWorks/burrow/issues/967)) and the fixes
before them have five causes between them, and every one is a missing owner.

1. **No one statement of state.** With several derivations, two of them
   disagree sooner or later: the view says a slot is held, the pass says it
   is free, and both are reading the same files.
2. **A wait doesn't say what ends it.** A job is parked, queued or awaiting
   because some code path put it there; what releases it is whatever other
   path remembers to. A park whose release nothing sends waits for ever, and
   nothing can tell, because the wait is a file, not a statement.
3. **Waking is an edge, not a level.** Work moves when the right message
   arrives. Lose it and the work sits until something unrelated happens by.
4. **Local decisions wait on remote calls.** GitHub refuses a listing, the
   pass gives up on the repo, and a slot that only this machine knows is
   free stays taken.
5. **The line lives in processes.** Admission is a negotiation between
   waiting processes, so it can't be read, tested as a whole or redeployed
   without ceremony; about 5,000 lines, fixtures and tests included, exist
   to move it across a deploy.

A sixth follows from the five: nobody can ask proj why. The answer is in
forty files, three journals and a reader's head.

## The design

Five rules, one for each cause.

### 1. One record per item

Every job, and every issue the inbox knows, has one record. It is the only
statement of where the item stands, and every view is that record printed:
`proj state`, `proj lifecycle`, the snapshot, the inbox's view, the
watchdog's items, the dashboard, `proj why`.

```rust
struct Record {
    stage: Stage,            // Queued, Running, Waiting, Ended(outcome), Closed
    wait: Option<Wait>,      // what it waits on; rule 2
    since: Time,             // when it entered this stage
    wake_at: Option<Time>,   // when to look again without an event
    overdue_at: Option<Time>,// past this, it is a stall
    next: String,            // what the next pass will do with it, in words
    history: Vec<Change>,    // the last stage changes, each with its reason
}
```

The field names are Conary's (`Next` in its flow has `wait_on`, `wait_until`
and `overdue_at`), so one reader serves both and a person reads a PR and a
job the same way.

One writer: the pass (rule 3). The record is the host's. A job reads its
own and never writes it; what a turn declares (`proj await`, `ask`, `done`)
is a request the host checks and records. Prompts, transcripts and logs stay
files: they are data, not state.

An issue's record is the same shape with the inbox's stages (unseen and why,
sorting, planned, ready, working, answered, closed). An issue the inbox
never enrolled has a record that says so and why, never nothing.

### 2. A wait is a type that says what ends it

`Wait` is closed: one variant per thing that can end a wait, and no path
can park, queue or await work without constructing one.

| Wait | It ends when | Which becomes true by | Overdue |
| --- | --- | --- | --- |
| `Slot(kind)` | a slot of that kind is free and this job is picked | a turn ending | the line's stall limit |
| `Job(id)` | that job's record is closed | that job ending | that job's `overdue_at` |
| `Usage(engine, until)` | the engine's meter allows a turn | the meter, or the clock | `until` |
| `Quota(until)` | GitHub's quota has reset | the clock | `until` |
| `Changed(target, seen)` | the PR or issue no longer looks like `seen` | a GitHub event | the await limit |
| `Answer(question)` | the question has an answer | the owner, or its default | the default's time |
| `Plan(step)` | the step before it merged or was dropped | a merge or a close | that step's `overdue_at` |
| `Policy(rule)` | the settings no longer deny the job | a settings change | never: it's a person's pause, and shown as one |
| `Login(engine)` | the engine's login works | the engines door | the owner's notice |
| `Lane(holder)` | the expedite lane is empty | its holder landing or leaving | the holder's `overdue_at` |

Two things make this more than a label.

- **The middle column is a fact the pass can read**, not a message someone
  must send. Nothing "fires" a release. The pass looks at the fact each
  time, so a wait whose condition holds is released by whichever pass comes
  next, whatever started it.
- **A cycle or a dead end is visible.** A job that waits on a PR whose own
  record waits on that job, a `Job(id)` whose job is gone, a `Slot` of a kind
  with no slots: each is a wait that can't end, and a rule can say so
  (rule 5).

A record read from before this type that names no wait is read as an orphan
and handed to the stall rule, never kept quietly.

### 3. One pass: facts in, steps out

```rust
fn pass(now: Time) {
    let facts = Facts::gather(now); // records, units, locks, meters,
                                    // settings, what GitHub last said
    for step in decide(&facts) {    // pure; no I/O in it
        apply(step);                // writes the record, then acts
    }
    book(earliest(wake_at, overdue_at));
}
```

`decide` is the whole of proj's judgement, in one pure function that tests
drive with made-up facts:

- **settle**: a unit that ended gets its outcome; its slot is free;
- **release**: a wait whose condition holds ends;
- **pause**: a job the settings deny is vacated, its wait `Policy`;
- **admit**: the ready jobs and the free slots go to `pick`, and the
  chosen start;
- **the inbox**: each issue's record and the last listing give its next
  step.

The pure pieces proj already has are the body of it and stay as they are:
`pick` (who gets a slot), `select` (which engine, from the meters), the
inbox's verdict steps, and the watchdog's item for a job.

The pass has two halves, in this order, and the first never waits for the
second.

- **The local half** reads and writes only this machine. It always
  finishes. Slots, verdicts, releases, pauses and admission are all here.
- **The remote half** talks to GitHub, a repo at a time. Reads come back as
  facts with their age; a refused read leaves the old facts in place and a
  `Quota` wait on that repo's reads, not on any job. Writes (labels,
  comments, closes, new issues) are effects in an outbox: each has a key
  that makes it safe to repeat, is recorded before it is tried and marked
  when it is done, and is retried when the quota allows. The inbox's
  verdict steps already work this way; this makes it the only way.

So a GitHub outage delays what needs GitHub, and nothing else.

### 4. The line is records, not processes

A turn waiting for a slot is a record with `Wait::Slot`. Admission is a step
of the pass: it gives `pick` the waiting records and the free slots, and
starts the units of the ones chosen. A turn's unit exists only while the
turn runs, and its slot is still a kernel lock it holds, so a turn that dies
frees it without anyone's help.

What goes with this: the queue of process ids, the polling, the turn that
re-execs itself on a deploy, the retry barrier and transfer units, and the
deployment protocol's document. A deploy becomes: the new binary reads the
records. The issues the inbox has ready and the turns in line become one
line, picked by one function, instead of the drain guessing at room.

### 5. Events mean look now, and a stall is a rule

Every event (a turn ending, a webhook, a settings change, `proj` being run)
does one thing: asks for a pass. An event carries no instructions, so a
duplicate is harmless and a lost one costs only the time until the next
pass, which the booked wake bounds. One wake is booked, the earliest
`wake_at` or `overdue_at` of any record, in place of the watcher's, the
inbox's and the policy's.

A stall is then a predicate over records, checked by the watchdog with one
rule:

- **released but not moving**: the wait's condition holds and a pass has
  run since;
- **can't end**: a cycle, or a wait on something that no longer exists;
- **overdue**: past `overdue_at`.

And nothing is a black box, because the answer to "why" is the record:

```
$ proj why burrow#969
working since 14:02 (38m): job a1f3, turn 2, running on slot 3
  next: nothing; its turn's deadline is 15:10
  13:58 ready -> working: picked ahead of 2 others (class first)
  13:41 sorting -> ready: verdict "one task", by job 9c2e
  13:40 unseen -> sorting: labelled by its owner
```

The same question about an issue the inbox passed over prints the reason it
did.

## What stays

- The sandbox, the engines and their sessions, and the turn's protocol with
  its agent (`await`, `ask`, `done`, `split`).
- Usage metering and the GitHub quota meter. What they hold becomes a
  `Wait`; how they measure doesn't change.
- Questions and their answers, and task planning's content
  ([task planning](task-planning.md)); a plan's gate on its next step
  becomes `Wait::Plan`, released by the step before it merging or being
  dropped.
- `pick`, `select` and the snapshot's schema, which gains nothing new: it
  was always meant to be the record.
- Slots as kernel locks.

## What goes

| Goes | Why it can |
| --- | --- |
| The four derivations in `job.rs`, the snapshot's, the inbox view's and pass's, and the lines `proj lifecycle` prints for the steward and burrow-upkeep to parse | rule 1: they read the record |
| The state markers in a job's directory, the policy's park, order and restoring files | rule 1: they are fields |
| Free-form holds; parking without a release | rule 2: `Wait` |
| The watcher, the policy unit, the turn-ended unit's fan-out of kicks, the three wake files, the lists of who is waiting on a sorting slot, the drain or a hold | rules 3 and 5: one pass, one wake |
| The inbox's local bookkeeping inside its GitHub pass | rule 3: the local half |
| The process line, the poll, the deploy handover, `retry.rs` and its fixtures, the retry barrier and transfer units | rule 4 |
| The watchdog's per-source timeouts | rule 5: one rule |

By the files they live in, that is on the order of 10,000 of the crate's
lines, tests included; the count is an estimate until the stages land.

At the end the crate is two, the runner and the inbox, with the inbox using
the runner through the door every customer uses. That split comes last: it
is what keeps the boundary once the design has made one.

## Conary

Conary's flow decides what a change needs next (review, fix, proof, judge,
land) from recorded facts, one pure decision per pass, and asks plugins to
run the jobs. proj decides when a job may run on this machine and says how
it went. The two keep their jobs:

- The flow's plugins start, read and stop proj jobs through the record
  (`proj state --json`), never through `lifecycle`'s lines. A caller waiting
  on a job reads the job's `overdue_at` and books its own deadline from it,
  so "waiting on a job with no deadline" can't be written.
- proj's pass has the flow's shape (facts and record in, steps and record
  out, effects with receipts, started by events and booked wakes), so the
  watchdog and the dashboard read both with one reader.
- When the steward is deleted, so are `lifecycle`, the `--owned` jobs made
  for it and their tests. The engines then become Conary job plugins
  ([#668](https://github.com/FieldmouseWorks/burrow/issues/668)), and what
  is left of proj is the records, the pass, the meters and the inbox.

proj does not become part of Conary, and issues do not become Conary
intents here. Either may be right later; neither is needed to stop the
stalls, and both would put two migrations on one machine at once.

## The stages

Each stage is one task and one PR, deletes what it replaces in that PR, and
leaves the pipeline running: a job started under the old code finishes under
the new, because the record can always be read from what the old code wrote.

| | Stage | Lands | Deletes | Issue |
| --- | --- | --- | --- | --- |
| 0 | Local before remote, where it hurts today | slot release, verdicts and parked jobs' places decided before GitHub is asked | | [#970](https://github.com/FieldmouseWorks/burrow/issues/970), [#943](https://github.com/FieldmouseWorks/burrow/issues/943), [#957](https://github.com/FieldmouseWorks/burrow/pull/957) |
| 1 | One derivation | a job's record, derived in one place; every reader on it; `proj why`; the record's history kept beside it. The same for the inbox's issues. | every reader's own derivation | [#969](https://github.com/FieldmouseWorks/burrow/issues/969), [#975](https://github.com/FieldmouseWorks/burrow/issues/975) |
| 2 | Waits are a type | `Wait`, built by every path that parks, queues or awaits | free-form holds and parks | [#972](https://github.com/FieldmouseWorks/burrow/issues/972) |
| 3 | One stall rule | the watchdog's rule over records | per-source timeouts | [#971](https://github.com/FieldmouseWorks/burrow/issues/971) |
| 4 | One pass | `proj pass`: the local half, then the remote half; one booked wake; every event asks for a pass | the watcher, the policy unit, the kick fan-out, the wake files | [#976](https://github.com/FieldmouseWorks/burrow/issues/976) |
| 5 | The record is the truth | every change of state is the pass writing the record | the markers | filed when 4 lands |
| 6 | The line is records | admission as a step of the pass | the process line, the handover, the retry transfer | filed when 5 lands |
| 7 | The inbox on the record | issue records, the effects outbox for every GitHub write, `Wait::Plan`; then the crate splits | the inbox's side files | filed when 6 lands |
| 8 | After the steward | engines as Conary job plugins | `lifecycle`, `--owned` | [#668](https://github.com/FieldmouseWorks/burrow/issues/668) |

The rules the stages keep:

- **One at a time.** Each builds on the one before, in the same files.
- **Stage 1 adds no second truth.** Its record is derived from today's
  markers on every read; only its history is stored. Stage 5 is where the
  record becomes what is written, and by then nothing else is read.
- **Anything that decides runs in shadow first.** Stages 4 and 6 compute
  the new decision beside the old one, report a difference as a watchdog
  item, and switch after it has been quiet, the way Conary's flow shadows
  the steward.
- **Stages 1 to 3 change none of proj's units and nothing the steward
  calls**, so they can land while Conary's flow is in shadow. Stage 4 onward
  waits until that cutover has settled: two engines swapped on one machine
  in one week is how you can't tell which one broke it.
- **Each stage's tests fail with its change reverted**, and stage 2 onward
  carry one test of the design itself: drop every event, run one pass, and
  everything whose condition holds has moved.

### Where it stands

Stage 0 is in flight. Nothing else has landed.

## What this doesn't do

- It adds no daemon and no timer. A pass is a unit burrow-events starts,
  like every handler today.
- It doesn't change what an agent sees: the commands a turn runs keep their
  names and meanings.
- It doesn't rewrite the engines, the sandbox or the meters.
- It doesn't fold the expedite lane
  ([#952](https://github.com/FieldmouseWorks/burrow/issues/952)) into
  something else: the lane is a class in `pick` and one `Wait`, and its
  rule that it holds one item is `Wait::Lane`, not a new list on disk.
