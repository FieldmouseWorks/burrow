# The shadow: Conary's landing flow beside the steward, deciding but never
# acting. After each pass of a repo in `burrow.autoland.shadow`,
# `cn flow compare` makes a dry pass of its PRs and sets each decision beside
# the `waiting: …` label and `review-settled` status the steward just left,
# in the pass's own journal (`shadow:` lines, disagreements first). It's how
# the flow is checked against the steward it replaces before any repo
# switches over (Conary's docs/flow.md, "Comparing decisions").
#
# Each repo's flow config is the steward's own settings for it, in the
# flow's terms (Conary's flow::Config). The judge's and renovate-sweep's seal
# keys stay out of it: they're secrets, and a config is a store path. Until
# the flow reads its producers' signed evidence instead (Conary#256, 5b), a
# PR waiting on a sealed verdict can show as a disagreement.
{ lib, pkgs, cfg, settings, ledger, steward }:
let
  minutes = m: m * 60;
  bot = lib.hasSuffix "[bot]";
  stewardLogin = "${cfg.app.slug}[bot]";
  flowConfig = repo:
    let
      rc = settings.defaults // (cfg.repos.${repo} or { });
      codex = rc.codex && settings.reviewers.codex.enable;
    in {
      trusted_authors = cfg.authors;
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
        proof_slots = (cfg.repos.${repo} or { }).proofSlots or null;
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
  # OWNER/NAME's config, at owner__name.json (a repo's name, lowercased as
  # systemd's %I may not keep it, is unique on GitHub).
  file = repo: lib.toLower (lib.replaceStrings [ "/" ] [ "__" ] repo) + ".json";
  configs = pkgs.linkFarm "autoland-shadow" (map (repo: {
    name = file repo;
    path = pkgs.writeText (file repo) (builtins.toJSON (flowConfig repo));
  }) cfg.shadow);
  # The ledger's environment, as `cn` reads it: its log, policy, signer and
  # fresh runners.
  env = lib.concatStrings (lib.mapAttrsToList (k: v: "export ${k}=${lib.escapeShellArg v}\n") ledger.env);
in
lib.optionalString (cfg.shadow != [ ]) (lib.getExe (pkgs.writeShellApplication {
  name = "autoland-shadow";
  runtimeInputs = [ pkgs.coreutils pkgs.gnused ];
  text = ''
    repo=$1
    name=''${repo,,}
    config=${configs}/''${name//\//__}.json
    # A pass of a repo that isn't shadowed (the hourly one, or one of
    # another repo) compares nothing.
    [ -e "$config" ] || exit 0
    ${env}
    export CONARY_TOKEN_COMMAND=${lib.escapeShellArg "${steward}/bin/autoland-steward token"}
    ${ledger.package}/bin/cn flow compare "$repo" --config "$config" | sed 's/^/shadow: /'
  '';
}))
