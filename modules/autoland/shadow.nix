# The shadow: Conary's landing flow beside the steward, deciding but never
# acting. Each pass of a repo in `burrow.autoland.shadow` that succeeds
# starts `autoland-shadow@REPO` (OnSuccess=), which runs `cn flow compare`:
# a dry pass of the repo's PRs, each decision set beside the `waiting: …`
# label and `review-settled` status the steward just left, in that unit's
# journal, disagreements first. It's how the flow is checked against the
# steward it replaces before any repo switches over (Conary's docs/flow.md,
# "Comparing decisions"). A unit of its own, so nothing it does (an error, a
# signal, its timeout) touches the pass's result.
#
# Each repo's flow config is the steward's own settings for it, in the
# flow's terms (Conary's flow::Config), at OWNER/NAME.json lowercased, as
# burrow-events names a pass's instance. The judge's and renovate-sweep's
# seal keys stay out of it: they're secrets, and a config is a store path.
# Until the flow reads its producers' signed evidence instead (Conary#256,
# 5b), a PR waiting on a sealed verdict can show as a disagreement.
{ lib, pkgs, cfg, settings, ledger, steward, user, credentials }:
let
  minutes = m: m * 60;
  bot = lib.hasSuffix "[bot]";
  stewardLogin = "${cfg.app.slug}[bot]";
  # A repo's overrides, however `repos` spells its name: GitHub's names
  # don't care about case.
  overrides = repo: lib.foldl' (acc: k: acc // lib.optionalAttrs (lib.toLower k == lib.toLower repo) cfg.repos.${k})
    { } (lib.attrNames cfg.repos);
  flowConfig = repo:
    let
      own = overrides repo;
      rc = settings.defaults // own;
      codex = rc.codex && settings.reviewers.codex.enable;
    in {
      # Whose PRs are the steward's authors': its people, and the bots
      # (their REST logins) whose PRs it lands too.
      trusted_authors = cfg.authors ++ map (b: "${b}[bot]") settings.bots;
      plugins = { };
      evidence =
        if rc.trainNegative != null then {
          signer = ledger.signerWorkflow;
          fresh = ledger.env.CONARY_FRESH_RUNNERS or null;
          require = rc.trainRequire;
          negative = rc.trainNegative;
        } else null;
      settings = {
        inherit (rc) train rounds;
        job_login = settings.machineryLogin;
        proof_slots = own.proofSlots or null;
        proof_timeout = minutes cfg.proofTimeoutMinutes;
        job_timeout = minutes cfg.jobTimeoutMinutes;
        reviewers = lib.optional codex "codex";
        review_authors = [ stewardLogin ];
        verdict_authors = [ stewardLogin ] ++ lib.optional (settings.machineryLogin != null) settings.machineryLogin;
        renovate_email = settings.renovateEmail;
        renovate_login = settings.renovate;
        batch_label = cfg.labels.batch;
        hold_label = cfg.labels.hold;
        first_label = cfg.labels.first;
        owner_label = cfg.labels.owner;
        people = lib.filter (a: !bot a) cfg.authors;
        require_ci = rc.requireCi;
        proofs = lib.mapAttrs (_: j: { Job = j; }) rc.proofJobs
          // lib.mapAttrs (_: c: { Run = [ settings.bash "-c" c ]; }) rc.proofRuns
          // lib.mapAttrs (_: a: { Build = a; }) rc.proofBuilds;
        settle_delay = minutes rc.settleDelay;
        handoff_after = minutes rc.handoffAfter;
        session_handoff_after = minutes rc.sessionHandoffAfter;
        stall_after = minutes rc.stallAfter;
        hold_stall_after = minutes rc.holdStallAfter;
        train_stall_after = minutes rc.trainStallAfter;
        renovate_merge_after = minutes rc.renovateMergeAfter;
        renovate_sweep = settings.majorKey != null;
      };
    };
  configs = pkgs.linkFarm "autoland-shadow" (map (repo: {
    name = "${lib.toLower repo}.json";
    path = pkgs.writeText "autoland-shadow.json" (builtins.toJSON (flowConfig repo));
  }) cfg.shadow);
  config = "${configs}/%I.json";
in
{
  inherit configs;
  # What a successful pass starts: the shadow, which skips a repo with no
  # config.
  onSuccess = lib.optional (cfg.shadow != [ ] && !cfg.dryRun) "autoland-shadow@%i.service";
  service = {
    description = "autoland: Conary's flow beside the steward's pass of %I";
    restartIfChanged = false;
    after = [ "network-online.target" ];
    wants = [ "network-online.target" ];
    unitConfig.ConditionPathExists = config;
    environment = ledger.env // {
      CONARY_TOKEN_COMMAND = "${steward}/bin/autoland-steward token";
    };
    serviceConfig = {
      Type = "oneshot";
      User = user;
      ExecStart = "${ledger.package}/bin/cn flow compare %I --config ${config}";
      # Well inside the next pass's: a compare is one read of each PR.
      TimeoutStartSec = "10min";
      Nice = 15;
      # The App's key, which the steward's token reads.
      LoadCredential = credentials;
    };
  };
}
