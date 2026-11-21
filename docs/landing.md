# Landing ownership

Each repository's PRs are landed by one controller: the steward
([autoland](../modules/autoland.nix)) or Conary's flow in conaryd
([Conary#313](https://github.com/FieldmouseWorks/Conary/issues/313)). The
steward lands every repo by default. The pieces are
[modules/autoland/landing.nix](../modules/autoland/landing.nix) and
[crates/burrow-landing](../crates/burrow-landing/src/lib.rs).

## The switch

```
settings landing OWNER/REPO conaryd    give the repo to conaryd
settings landing OWNER/REPO steward    hand it back
settings show                          who holds each repo, and any handover under way
```

It's a live setting in the settings overlay, changed through the same
door as `pause` and `allow` (the issue door takes `landing OWNER/REPO ...` too) and
logged the same way. No deploy is needed in either direction, so the way back
never waits on the pipeline being left. `conaryd` is refused for a repo that
isn't in `burrow.landing.conaryd` (a deploy option). A repo may only be listed
there once it's shadowed (`burrow.autoland.shadow`). Being listed gives conaryd
nothing; only the setting does. Taking a repo off the list at a deploy hands
it back to the steward through the same handover. That works even when it's
the last repo and conaryd no longer runs: burrow-landing runs wherever the
ledger does, and then waits only for the workers conaryd left.

## Who lands, and when

The record (`landing/owners.json` in the state dir) says who holds each
repo now. Only burrow-landing writes it. Everything that could land reads it:

- The steward reads it through its admission `decision`. It takes a landing
  action (a PR's step, a job, a proof, a merge, the train, deleting branches
  by what PRs use, a `settings land` handoff) only in a repo it holds with no
  handover under way. A repo it doesn't hold still gets its settings, hook
  and upkeep. If the record can't be read, the decision fails, and the steward
  lands nothing.
- burrow-events reads it at each delivery, kick and wake-up. The steward's
  handlers take only the repos it holds. Their wake-ups booked for a repo it
  has let go of are spent without a pass. Deliveries for a repo conaryd holds
  go to conaryd's listener exactly as GitHub signed them.
- conaryd reads its events file. It lists the repos the record gives
  conaryd whose automated work is allowed, and no others, each with its own
  flow configuration (`flow_config`), the one its shadow reads. Its
  `evidence.fresh` is the machine's fresh runners
  (`burrow.dispatch.fresh`) for a repo in their `repos` or whose
  `trainFresh` setting is true (a private repo, on the runners dispatch
  registers for it), and null, GitHub-hosted runners only, for every other.
- renovate-run ([crates/burrow-renovate](../crates/burrow-renovate/src/run.rs))
  reads it as each run of Renovate starts. Renovate merges only in a repo
  the steward lands; anywhere else, a handover under way included, it opens
  and updates PRs and merges none, however its config or a security fix
  asks ([patches/renovate-no-merge.patch](../patches/renovate-no-merge.patch)).
  If the record can't be read, it merges nowhere and the run fails.

conaryd runs as the flow's user, the controller where there is one
(`burrow.ledger.controllerUser`). A flow configuration with a train or
evidence has it sign the ledger, and the ledger module gives the controller
that, as its option says.

## A handover

A handover has two steps, so the steward and conaryd never land in a repo
at the same time:

1. **The handover starts.** The record names who the repo goes to (`to`)
   and counts a new `generation`. From then on, neither controller lands
   there.
   - The steward lets go at its next pass, which burrow-landing kicks. It
     stops every job and proof build its PRs' states name. Once none is
     alive, it writes the generation to `autoland/landing-released.json`.
   - conaryd lets go by being taken off its events file. It passes no repo
     it isn't given.
2. **The new owner takes the repo** once the one letting go is quiet:
   - The steward is quiet when its marker names this generation.
   - conaryd is quiet once three things hold. It has started again since
     the events file was last published without the repo, which a
     publication serial shows; matching contents isn't enough, because a
     quick hand-over-and-back republishes the same file. The serial also
     names the hash of the file it counts, so a file replaced by a run cut
     short before its count was written is counted at the next run, and
     never taken for one conaryd has started on. A pass reads its
     repos before it takes the repo's lock, so until the restart, a pass
     that read the old file could still start. No pass holds the flow's repository lock. Every
     worker it launched there has exited. The last two are Conary's own
     locks, and they're waited on, never guessed at.

   Then the record gives the repo to the new owner, and burrow-landing wakes
   it:
   - conaryd: its events file changes, which restarts it, and its start
     kicks every repo it's given.
   - The steward: a kick of the repo's pass.

conaryd runs only while it's given a repo. Next to its events file,
burrow-landing keeps `landing/conaryd-given`, there exactly while the file
gives conaryd one; conary-flow starts only while it's there, and a change of
it is a publication of its own, counted even when the file's repos didn't
change. It holds the count of the publication that put it there, so one a
run cut short before its count is still counted at the next run. So conaryd starts with the first repo it's given, and stops for good
at the restart that takes off its last one. A machine that lists repos in
`burrow.landing.conaryd` but gives conaryd none never starts it, and so
never needs its credentials (the hooks' secret and the App's key). Given a
repo without them, conaryd fails, and says so.

Setting the owner back before a handover finishes ends the handover, and
the owner that was letting go carries on. A release for the handover that
was ended counts for nothing, because its generation is spent.
burrow-landing reads everything again on each run, which happens at each
change of the setting, the steward's marker or conaryd's restart, at each
deploy, at boot, and five minutes after the last run as a backstop. A run
goes again if what it reads changed while it ran, and a failed run is
restarted. A run that was cut short carries on from the record.

The repo's ledger state (its changes, evidence and train) stays where it
is. Both controllers read the same ledger.

## Pauses

Pausing is independent of ownership. A switch in ownership doesn't lift
a pause, and a pause doesn't move ownership:

- `settings pause OWNER/REPO` stops landing under either owner, and proj
  parks the repo's Claude jobs and proofs. Under the steward, its decision
  holds every landing. Under conaryd, the repo is taken off conaryd's events
  file while the pause lasts, so conaryd passes it no more. Its workers
  aren't vacated, though; their jobs are proj's to park. With the only
  repo conaryd is given paused, conaryd stops until the pause ends (it runs
  only while it's given a repo). A pipeline-only
  hold that ends writes nothing, so the repo is given back to conaryd at
  burrow-landing's next run.
- The flow's own switch, `cn flow pause OWNER/REPO --reason TEXT`, run as the
  flow's user, is independent of burrow's. It needs no conaryd, GitHub or
  deploy, and it vacates the flow's workers at conaryd's next pass (Conary's
  [docs/flow.md](https://github.com/FieldmouseWorks/Conary/blob/main/docs/flow.md#pause-and-hold)).
  burrow never writes or lifts it.
- A pause names a whole repository, on both sides. The flow's pause leaves
  room for a narrower scope as another kind of the same pause, but that's not
  built yet. Under conaryd there's no handoff (`settings land` is refused for
  a repo the steward doesn't hold). So every change in a repo conaryd lands
  gets everything landing needs: its review, its proofs and its judgment
  (lesson `scope-below-a-repo`).

A late review in a repo conaryd lands is asked for through the owner's door
(burrow-review-ask), as the steward asks: the flow config's `review_ask`
names its command and `burrow.autoland.askAsOwner.repos`, and the command
starts the door as burrow.user's own unit. The door exits 0 only once the
head is asked, so a no that may not last (the day's cap, a hold) leaves the
flow to ask again on a later pass. The command reaches burrow.user's
manager only from that user, so conaryd must run as burrow.user (no `burrow.ledger.controllerUser`) in a repo the door asks in,
and a build that breaks this is refused.

## What it doesn't do yet

- The overlay's `landing` entry is new. A burrow from before it refuses an
  overlay that has one. Hand every repo back to the steward
  (`settings landing OWNER/REPO steward`) before rolling back past it.
