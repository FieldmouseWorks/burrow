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
it back to the steward through the same handover.

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
- conaryd reads its events file, which lists the repos the record gives it
  and no others.

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
   - conaryd is quiet when no pass holds the flow's repository lock and
     every worker it launched there has exited (Conary's own locks). Those
     workers are waited on, never guessed at.

   Then the record gives the repo to the new owner, and burrow-landing wakes
   it:
   - conaryd: its events file changes, which restarts it, and its start
     kicks every repo it's given.
   - The steward: a kick of the repo's pass.

Setting the owner back before a handover finishes ends the handover, and
the owner that was letting go carries on. A release for the handover that
was ended counts for nothing, because its generation is spent.
burrow-landing reads everything again on each run, at each change of the
setting or the marker, at each deploy and at boot. A run that was cut short
carries on from the record.

The repo's ledger state (its changes, evidence and train) stays where it
is. Both controllers read the same ledger.

## Pauses

Pausing is independent of ownership. A switch in ownership doesn't lift
a pause, and a pause doesn't move ownership:

- `settings pause OWNER/REPO` parks the repo's Claude jobs and proofs under
  either owner, because proj refuses them. Under the steward it also holds
  every landing. Under conaryd it doesn't write the flow's pause. To stop
  conaryd landing in a repo, use the flow's own switch: `cn flow pause OWNER/REPO
  --reason TEXT`, as the flow's user. It needs no conaryd, GitHub or deploy
  (Conary's [docs/flow.md](https://github.com/FieldmouseWorks/Conary/blob/main/docs/flow.md#pause-and-hold)).
- A pause names a whole repository, on both sides. The flow's pause leaves
  room for a narrower scope as another kind of the same pause, but that's not
  built yet. Under conaryd there's no handoff (`settings land` is refused for
  a repo the steward doesn't hold). So every change in a repo conaryd lands
  gets everything landing needs: its review, its proofs and its judgment
  (lesson `scope-below-a-repo`).

## What it doesn't do yet

- conaryd reads one flow configuration for every repo it lands. Any repos
  given to one conaryd must therefore render the same one, and a build that
  breaks this is refused.
- conaryd runs as the flow's user. A flow configuration with a train or
  evidence has it sign the ledger, so that user needs the ledger's key.
  [Conary#314](https://github.com/FieldmouseWorks/Conary/issues/314)
  provisions it along with the producers' keys.
