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
# burrow-events names a pass's instance. The seal keys stay out of it:
# they're secrets, and a config is a store path. The judge's is named by
# its file instead (`judge_key_file`, which Conary reads at each load):
# autoland-flow-keys's copy of its credential, which only the flow's
# controller can read. renovate-sweep's (`major_key`) isn't given to the flow yet.
# Until the flow reads its producers' signed evidence instead (Conary#256,
# 5b), a PR waiting on a sealed verdict can show as a disagreement.
#
# Each enabled plugin (burrow.autoland.plugins) is in it: its executable in
# `plugins`, what it's asked to run with in `execution`, and, where the repo
# has evidence, its producer in the producers file `evidence.producers`
# names, trusted for its checks there alone (none, and it's left out). With
# none enabled, none of the three is. A role's choice is the one the
# settings source declares, since a build reads no live settings: a role
# set live (`proj settings role` or `model`) to anything else has its jobs
# refuse the flow's choice, rebuilt or not (burrow#1274).
{ lib, pkgs, cfg, settings, ledger, steward, credentials, flowFiles }:
let
  minutes = m: m * 60;
  bot = lib.hasSuffix "[bot]";
  stewardLogin = "${cfg.app.slug}[bot]";
  # The plugins turned on, by the kind the flow knows each by.
  enabled = lib.filterAttrs (_: p: p.enable) { inherit (cfg.plugins) fix job judge proof; };
  # The flow's seal keys, in a directory of the flow's controller alone
  # for as long as a unit of its own is up: systemd reads each key file as
  # root and hands it to the unit as a credential, which the unit, as the
  # controller, copies into its runtime directory. Not the credential
  # itself: a unit's credentials are mounted in its own namespace, which no
  # other unit sees. Every unit that runs the flow (the shadow's and
  # conaryd's) requires it, as its config names the key's file there. None
  # with the judge off: no unit.
  keysUnit = "autoland-flow-keys";
  seals = lib.optionalAttrs (enabled ? judge) { judge-seal = enabled.judge.sealFile; };
  keyFile = name: "/run/${keysUnit}/${name}";
  needs = lib.optional (seals != { }) "${keysUnit}.service";
  # The judge's: what the configs name, and what its producer checks they
  # name, holding as it does the same sealFile as its own credential.
  judgeKey = keyFile "judge-seal";
  # The producers a repo's evidence trusts, and the checks each may attest
  # (Conary's docs/m6-plugins.md, "Who produces what"): the judge its
  # judgment, the proof producer the repo's proofs. One with none there
  # (the proof producer, in a repo with no proofs, as every train's is yet)
  # isn't trusted there at all.
  producers = proofs: lib.filter (p: p.checks != [ ]) (lib.optional (enabled ? judge) {
      name = enabled.judge.producer;
      key = enabled.judge.publicKey;
      checks = [ "conary/judgment" ];
    } ++ lib.optional (enabled ? proof) {
      name = enabled.proof.producer;
      key = enabled.proof.publicKey;
      checks = lib.attrNames proofs;
    });
  # Read by Conary's own loader as it's built, since `cn flow settings`
  # doesn't open the file the config names: a producer it would refuse (a
  # key that isn't one, or one another producer holds) fails the machine's
  # build.
  producersFile = list: pkgs.runCommand "autoland-producers.json" {
    producers = builtins.toJSON list;
    passAsFile = [ "producers" ];
  } ''
    ${flowFiles}/bin/burrow-flow-files producers "$producersPath"
    cp "$producersPath" "$out"
  '';
  # A repo's overrides, however `repos` spells its name: GitHub's names
  # don't care about case.
  overrides = repo: lib.foldl' (acc: k: acc // lib.optionalAttrs (lib.toLower k == lib.toLower repo) cfg.repos.${k})
    { } (lib.attrNames cfg.repos);
  flowConfig = repo:
    let
      own = overrides repo;
      rc = settings.defaults // own;
      codex = rc.codex;
      proofs = lib.mapAttrs (_: j: { Job = j; }) rc.proofJobs
        // lib.mapAttrs (_: c: { Run = [ settings.bash "-c" c ]; }) rc.proofRuns
        // lib.mapAttrs (_: a: { Build = a; }) rc.proofBuilds;
      trusted = producers proofs;
    in {
      # Whose PRs are the steward's authors': its people, and the bots
      # (their REST logins) whose PRs it lands too.
      trusted_authors = cfg.authors ++ map (b: "${b}[bot]") settings.bots;
      plugins = lib.mapAttrs (_: p: "${p.plugin}") enabled;
      evidence =
        if rc.trainNegative != null then {
          signer = ledger.signerWorkflow;
          fresh = ledger.freshRunners repo;
          require = rc.trainRequire;
          negative = rc.trainNegative;
        } // lib.optionalAttrs (trusted != [ ]) {
          producers = producersFile trusted;
        } else null;
    } // lib.optionalAttrs (cfg.askAsOwner.door != null) {
      # Where the connector takes no App's ask, the flow asks for a late
      # review through the owner's door (autoland/review-ask.nix), as the
      # steward does: its command, run with REPO PR HEAD, and its repos.
      # Absent with no door: the flow then asks with its own comment.
      review_ask = { inherit (cfg.askAsOwner.door) command repos; };
    } // {
      settings = {
        inherit (rc) train rounds;
        # The most PRs a train candidate holds, as the steward's `cn train
        # step --size` says it.
        train_size = rc.trainSize;
        job_login = settings.machineryLogin;
        proof_slots = own.proofSlots or null;
        proof_timeout = minutes cfg.proofTimeoutMinutes;
        job_timeout = minutes cfg.jobTimeoutMinutes;
        reviewers = lib.optional codex "codex";
        # Codex's GitHub connector posts the reviews: the ledger's own list.
        review_authors = ledger.reviewAuthors;
        # The flow's own account: review-settled, the conary/train status and
        # its answers are read only from it.
        flow_authors = [ stewardLogin ];
        verdict_authors = [ stewardLogin ] ++ lib.optional (settings.machineryLogin != null) settings.machineryLogin;
        # No job engine's commits go to a judge on their own: there are no
        # Codex jobs. One by none of the bots' addresses is someone editing a
        # dependency update, as the steward reads a PR's commits (touches.py,
        # burrow-upkeep's commits).
        judge_engines = [ ];
        bot_emails = [ settings.dependabotEmail ];
        renovate_email = settings.renovateEmail;
        renovate_login = settings.renovate;
        hold_label = cfg.labels.hold;
        # The steward's one priority: its first label, ahead of the rest.
        # It knows no other level, so Conary's own `priority: …` labels are
        # left out, and an unlabelled PR is at Conary's default (medium).
        priority_labels = { ${cfg.labels.first} = "high"; };
        owner_label = cfg.labels.owner;
        people = lib.filter (a: !bot a) cfg.authors;
        require_ci = rc.requireCi;
        inherit proofs;
        settle_delay = minutes rc.settleDelay;
        handoff_after = minutes rc.handoffAfter;
        session_handoff_after = minutes rc.sessionHandoffAfter;
        # A first PR is overdue in half an hour (or stallAfter, if that's
        # sooner), the rest at stallAfter: the deadlines burrow#960 sets out.
        # Until the steward keeps them too, a first PR waiting between the
        # two is overdue to the flow alone.
        stall_after = {
          high = minutes (lib.min 30 rc.stallAfter);
          medium = minutes rc.stallAfter;
          low = minutes rc.stallAfter;
        };
        hold_stall_after = minutes rc.holdStallAfter;
        train_stall_after = minutes rc.trainStallAfter;
        renovate_sweep = settings.majorKey != null;
      } // lib.optionalAttrs (seals ? judge-seal) {
        # The judge's seal key, its sealFile, as the flow's controller alone
        # reads it.
        judge_key_file = judgeKey;
      };
    };
  # Loaded by the machine's own cn as it's built (`cn flow settings`), as
  # the ledger's policy is: Conary refuses a key it doesn't know or a value
  # of the wrong type, so a burrow and a Conary out of step on the flow's
  # settings fail the machine's build, rather than every shadow once it's
  # live.
  #
  # Each enabled plugin's choice (`execution`) is its own command's, run
  # here: a role's is read from the settings source, as its jobs read it.
  flowFile = repo: let
    kinds = lib.attrNames enabled;
    rendered = flowConfig repo;
  in pkgs.runCommand "autoland-shadow.json" {
    config = builtins.toJSON rendered;
    passAsFile = [ "config" ];
    nativeBuildInputs = lib.optional (kinds != [ ]) pkgs.jq;
    # The producers its evidence trusts, as rendered, before Conary's loader
    # reads them: for a check to hold the loader to them.
    passthru.producers = rendered.evidence.producers or null;
    # The plugins it names, by kind, read with no build.
    passthru.plugins = rendered.plugins;
    # A train with no evidence, which Conary's train can't step on.
    passthru.trainWithoutEvidence = rendered.settings.train && rendered.evidence == null;
  } ''
    ${lib.optionalString (kinds != [ ]) ''
      ${lib.concatMapStrings (k: "${enabled.${k}.choice} > choice-${k}.json\n") kinds}
      jq -cS ${lib.concatMapStrings (k: "--slurpfile ${k} choice-${k}.json ") kinds} \
        '. + {execution: {${lib.concatMapStringsSep ", " (k: "${k}: $" + k + "[0]") kinds}}}' "$configPath" > config.json
      configPath=config.json
    ''}
    HOME=$TMPDIR ${ledger.package}/bin/cn flow settings --config "$configPath" >/dev/null
    cp "$configPath" "$out"
  '';
  configs = pkgs.linkFarm "autoland-shadow" (map (repo: {
    name = "${lib.toLower repo}.json";
    path = flowFile repo;
  }) cfg.shadow);
  config = "${configs}/%I.json";
in
{
  inherit configs needs judgeKey;
  # One repo's, by its name.
  file = flowFile;
  # The unit holding the flow's seal keys, null with none.
  keys = if seals == { } then null else {
    name = keysUnit;
    service = {
      description = "autoland: the flow's seal keys, for its controller alone";
      wantedBy = [ "multi-user.target" ];
      # One of the flow's own units (burrow.ledger keeps the controller to
      # those).
      environment = ledger.flow.env;
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        User = ledger.flow.user;
        LoadCredential = lib.mapAttrsToList (name: file: "${name}:${file}") seals;
        # Gone, keys and all, when the unit stops.
        RuntimeDirectory = keysUnit;
        RuntimeDirectoryMode = "0700";
        # %d: its credentials' directory.
        ExecStart = map (name: "${pkgs.coreutils}/bin/install -m 0400 %d/${name} ${keyFile name}") (lib.attrNames seals);
      };
    };
  };
  # What a successful pass starts: the shadow, which skips a repo with no
  # config.
  onSuccess = lib.optional (cfg.shadow != [ ] && !cfg.dryRun) "autoland-shadow@%i.service";
  service = {
    description = "autoland: Conary's flow beside the steward's pass of %I";
    restartIfChanged = false;
    after = [ "network-online.target" ] ++ needs;
    wants = [ "network-online.target" ];
    requires = needs;
    unitConfig.ConditionPathExists = config;
    environment = ledger.env // ledger.flow.env // {
      CONARY_TOKEN_COMMAND = "${steward}/bin/autoland-steward token";
    };
    serviceConfig = {
      Type = "oneshot";
      # The flow's controller where it has one (burrow.ledger.controllerUser).
      User = ledger.flow.user;
      ExecStart = "${ledger.package}/bin/cn flow compare %I --config ${config}";
      # Well inside the next pass's: a compare is one read of each PR.
      TimeoutStartSec = "10min";
      Nice = 15;
      # The App's key, which the steward's token reads.
      LoadCredential = credentials;
    };
  };
}
