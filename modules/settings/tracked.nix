# The flake's value of each option burrow-settings' schema declares to be set
# in the flake (crates/burrow-settings/src/schema.rs, the `flake(...)`
# entries), by its name there, which is its path under `burrow.`: the value,
# and whether the machine's flake set it (a definition above the option's own
# default) or left the default. Null for an option whose module the machine
# doesn't have. proj's config carries it for `proj settings show`, which
# fails on a name here the schema doesn't declare, on one it declares that
# isn't here, and on a default here that isn't the schema's, so the two lists
# can't drift apart unseen (checks/settings.nix).
{ lib, options }:
let
  read = name:
    let opt = lib.attrByPath ([ "burrow" ] ++ lib.splitString "." name) null options;
    in if opt == null then null else {
      inherit (opt) value;
      # mkOptionDefault's priority is the option's own default.
      set = opt.highestPrio < (lib.mkOptionDefault { }).priority;
    };
in
lib.genAttrs [
  "proj.maxJobs"
  "proj.pipelineSlots"
  "proj.maxJobsPerProject"
  "proj.codex.slots"
  "proj.usageTrickle"
  "proj.usage.paid"
  "proj.paused"
  "proj.inbox.triage.drain"
  "proj.inbox.triage.drainJobs"
  "proj.inbox.triage.atOnce"
  "autoland.rounds"
  "autoland.handoffAfterMinutes"
  "autoland.sessionHandoffAfterMinutes"
  "autoland.settleDelayMinutes"
  "autoland.stallAfterMinutes"
  "autoland.trainStallAfterMinutes"
  "autoland.holdStallAfterMinutes"
  "autoland.renovateMergeAfterMinutes"
  "autoland.jobTimeoutMinutes"
  "autoland.jobIdleMinutes"
  "autoland.proofTimeoutMinutes"
  "autoland.baselinePerPass"
  "autoland.repos"
  "autoland.reviewers.codex.enable"
  "autoland.reviewers.codex.effort"
  "autoland.reviewers.codex.parallel"
  "autoland.reviewers.timeoutMinutes"
  "autoland.reviewers.skipPaths"
] read
