# The flake's value of each option burrow-settings' schema declares to be set
# in the flake (crates/burrow-settings/src/schema.rs, the `flake(...)`
# entries), by its name there, which is its path under `burrow.`: the value,
# whether the machine's flake set it (a definition above the option's own
# default) or left the default, and the module's own default (given the
# machine's other values, for one worked out from them). Null for an option
# whose module the machine doesn't have. proj.nix puts it in /etc for `proj
# settings show`, which fails on a name here the schema doesn't declare, on
# one it declares that isn't here, and on a module default that isn't the
# schema's, overridden or not, so the two can't drift apart unseen
# (checks/settings.nix).
{ lib, options }:
let
  read = name:
    let opt = lib.attrByPath ([ "burrow" ] ++ lib.splitString "." name) null options;
    in if opt == null then null else {
      inherit (opt) value default;
      # mkOptionDefault's priority is the option's own default.
      set = opt.highestPrio < (lib.mkOptionDefault { }).priority;
    };
in
lib.genAttrs [
  "authors"
  "proj.maxJobs"
  "proj.pipelineSlots"
  "proj.maxJobsPerProject"
  "proj.codex.slots"
  "proj.usageTrickle"
  "proj.usage.paid"
  "settings.allowed"
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
