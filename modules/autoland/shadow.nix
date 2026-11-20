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
# they're secrets, and a config is everyone's to read. Each is named by its
# file instead (`judge_key_file`, `major_key_file`, which Conary reads at
# each load): autoland-flow-keys's copy of its credential, which only the
# flow's controller can read. The judge's is its sealFile; renovate-sweep's
# its verdict.key, which the sweep makes as it runs, so the keys unit has
# the sweep make it first, and copies it again whenever it changes.
# Until the flow reads its producers' signed evidence instead (Conary#256,
# 5b), a PR waiting on a sealed verdict can show as a disagreement.
#
# Each enabled plugin (burrow.autoland.plugins) is in it: its executable in
# `plugins`, what it's asked to run with in `execution`, and, where the repo
# has evidence, its producer in the producers file `evidence.producers`
# names, trusted for its checks there alone (none, and it's left out). With
# none enabled, none of the three is.
#
# A build renders each config's base, held to Conary's loader, with the
# choices that are a plugin's own (the proof plugin's). A choice that's a
# role's (fix, judge, job) is the settings' to make, live, and model ids
# live only in the role overlay, so no build can render one: the machine
# does. `autoland-flow-configs` writes each base with each role's choice
# into the live directory every reader of the flow's configs reads (the
# flow, its shadow and the plugins), at boot, at each switch and whenever
# the role settings change (crates/burrow-flow-files, render.rs). conaryd
# reads its config at each pass, so `proj settings role`, `model` or `proj
# engines use` takes effect with no deploy (burrow#1393). A role whose
# choice its jobs would refuse (no model, or one its engine doesn't offer)
# fails the render, which then writes nothing.
{ lib, pkgs, cfg, settings, ledger, credentials, tokenCommand, flowFiles, config }:
let
  inherit (config.burrow) stateDir;
  settingsSource = config.burrow.settings;
  minutes = m: m * 60;
  bot = lib.hasSuffix "[bot]";
  stewardLogin = "${cfg.app.slug}[bot]";
  # The plugins turned on, by the kind the flow knows each by.
  enabled = lib.filterAttrs (_: p: p.enable) { inherit (cfg.plugins) fix job judge proof; };
  # Those whose choice is a role's, rendered live, and those whose choice
  # is their own, rendered by the build.
  roleRun = lib.filterAttrs (_: p: p ? role) enabled;
  ownChoice = lib.filterAttrs (_: p: !(p ? role)) enabled;
  # Where the flow's configs are read, as the machine renders them:
  # everyone's to read and, as with the store path each base is, none of
  # its readers' to write. A producer takes a proof's recipe from here and
  # signs it, so not the flow's controller's either, which asks for them:
  # only the renderer's, a user that does nothing else.
  live = "${stateDir}/flow-configs";
  renderUnit = "autoland-flow-configs";
  # The flow's seal keys, in a directory of the flow's controller alone
  # for as long as a unit of its own is up: systemd reads each key file as
  # root and hands it to the unit as a credential, which the unit, as the
  # controller, copies into its runtime directory. Not the credential
  # itself: a unit's credentials are mounted in its own namespace, which no
  # other unit sees. Every unit that runs the flow (the shadow's and
  # conaryd's) requires it, as its config names the key's file there. None
  # with the judge and renovate-sweep off: no unit. They require the
  # flow's state too, carried to the flow's user (burrow.ledger.flow.needs).
  keysUnit = "autoland-flow-keys";
  seals = lib.optionalAttrs (enabled ? judge) { judge-seal = enabled.judge.sealFile; }
    # renovate-sweep's, where there's a flow to read it: a shadowed repo
    # (conaryd lands only those).
    // lib.optionalAttrs (settings.majorKey != null && cfg.shadow != [ ]) { major-seal = settings.majorKey; };
  # What makes renovate-sweep's key, before the keys unit takes it
  # (updates.nix), and what has it taken again once it changes: the sweep
  # makes a new one for one that isn't a key, and its old one's copy would
  # check no verdict the sweep hands out after.
  sweepKey = "renovate-sweep-key.service";
  keysAgain = "${keysUnit}-again";
  keyFile = name: "/run/${keysUnit}/${name}";
  needs = ledger.flow.needs ++ lib.optional (seals != { }) "${keysUnit}.service";
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
      people = lib.filter (a: !bot a) cfg.authors;
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
        # Whose sealed judge and major verdicts count: the steward's App and,
        # where there's a machinery App, its bot (which renovate-sweep's jobs
        # and the judge producer post as); without one, the machine user's own
        # GitHub login, one of `people`, as the steward's `ours` reads it.
        verdict_authors = [ stewardLogin ]
          ++ (if settings.machineryLogin != null then [ settings.machineryLogin ] else people);
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
        inherit people;
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
      } // lib.optionalAttrs (seals ? major-seal) {
        # renovate-sweep's seal key, its verdict.key, likewise: the one the
        # majors' verdicts it seals in a repo conaryd lands are v2 under.
        major_key_file = keyFile "major-seal";
      };
    };
  # Loaded by the machine's own cn as it's built (`cn flow settings`), as
  # the ledger's policy is: Conary refuses a key it doesn't know or a value
  # of the wrong type, so a burrow and a Conary out of step on the flow's
  # settings fail the machine's build, rather than every shadow once it's
  # live.
  #
  # The base: each plugin whose choice is its own has it in `execution`,
  # its command's, run here. A role's is the machine's to render (above).
  flowFile = repo: let
    kinds = lib.attrNames ownChoice;
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
      ${lib.concatMapStrings (k: "${ownChoice.${k}.choice} > choice-${k}.json\n") kinds}
      jq -cS ${lib.concatMapStrings (k: "--slurpfile ${k} choice-${k}.json ") kinds} \
        '. + {execution: {${lib.concatMapStringsSep ", " (k: "${k}: $" + k + "[0]") kinds}}}' "$configPath" > config.json
      configPath=config.json
    ''}
    HOME=$TMPDIR ${ledger.package}/bin/cn flow settings --config "$configPath" >/dev/null
    cp "$configPath" "$out"
  '';
  base = pkgs.linkFarm "autoland-shadow" (map (repo: {
    name = "${lib.toLower repo}.json";
    path = flowFile repo;
  }) cfg.shadow);
  # What renders the live configs from the base (crates/burrow-flow-files,
  # render.rs): the settings source, whose role overlay it reads, and each
  # role-run plugin's role.
  manifest = pkgs.writeText "autoland-flow-configs.json" (builtins.toJSON {
    settings = settingsSource.file;
    inherit base;
    out = live;
    roles = lib.mapAttrs (_: p: p.role) roleRun;
  });
  # The role settings, where `proj settings role`, `model`, `offer` and
  # `proj engines use` write them (burrow-settings' roles_file).
  rolesFile = "${lib.removeSuffix ".json" settingsSource.effective.overlay}.roles.json";
  instanceConfig = "${live}/%I.json";
  render = lib.optional (cfg.shadow != [ ]) "${renderUnit}.service";
in
{
  inherit needs judgeKey base live;
  # One repo's base, by its name, as the build renders it.
  file = flowFile;
  # One repo's live config, by its name: what the flow reads.
  liveFile = repo: "${live}/${lib.toLower repo}.json";
  # The unit that renders them, for their readers to come after: none with
  # no shadowed repo, which has none to render.
  inherit render;
  # The renderer: at boot and at each switch as multi-user.target starts
  # it (a switch starts the target again once systemd has reloaded, so on
  # the new base: a path unit on /run/current-system would fire before that
  # and run the old one), and at each change to the role settings, by its
  # path unit. Not a timer: nothing else changes what it writes, and a
  # change that comes as it runs is read again before it ends (render.rs).
  renderer = let on = cfg.shadow != [ ]; in {
    # The renderer's own user, and the directory only it writes.
    users = lib.optionalAttrs on {
      users.${renderUnit} = { isSystemUser = true; group = renderUnit; };
      groups.${renderUnit} = { };
    };
    tmpfiles = lib.optionalAttrs on { ${live}.d = { mode = "0755"; user = renderUnit; group = renderUnit; }; };
    services = lib.optionalAttrs on { ${renderUnit} = {
      description = "autoland: render the flow's configs with each role's live choice";
      wantedBy = [ "multi-user.target" ];
      # No start limit: a few role changes in a row are a few starts, and
      # one refused by the limit fails the path unit too, which then hears
      # no change again. It writes nothing it's started by, so can't loop.
      startLimitIntervalSec = 0;
      serviceConfig = {
        Type = "oneshot";
        User = renderUnit;
        NoNewPrivileges = true;
        ExecStart = "${flowFiles}/bin/burrow-flow-files render ${manifest}";
        UMask = "0022";
        TimeoutStartSec = "1min";
      };
    }; };
    paths = lib.optionalAttrs on { ${renderUnit} = {
      wantedBy = [ "multi-user.target" ];
      pathConfig.PathChanged = [ rolesFile ];
    }; };
  };
  # The unit holding the flow's seal keys, none with none; with
  # renovate-sweep's among them, the path unit that has it take them again
  # when the sweep's changes, and the unit that path starts.
  keys = let
    again = seals ? major-seal;
  in {
    paths = lib.optionalAttrs again { ${keysAgain} = {
      wantedBy = [ "multi-user.target" ];
      pathConfig = { PathChanged = [ seals.major-seal ]; Unit = "${keysAgain}.service"; };
    }; };
    services = lib.optionalAttrs again {
      # A restart of a unit running ones that require it restarts them too,
      # on the new key: conaryd's and the shadow's, which read it at each
      # pass anyway. Nothing when it isn't up: its start takes the key.
      ${keysAgain} = {
        description = "autoland: the flow's seal keys, taken again for renovate-sweep's new one";
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "${config.systemd.package}/bin/systemctl try-restart ${keysUnit}.service";
        };
      };
    } // lib.optionalAttrs (seals != { }) { ${keysUnit} = {
      description = "autoland: the flow's seal keys, for its controller alone";
      wantedBy = [ "multi-user.target" ];
      # renovate-sweep's key made, if it has none, before it's taken.
      requires = lib.optional again sweepKey;
      after = lib.optional again sweepKey;
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
    }; };
  };
  # What a successful pass starts: the shadow, which skips a repo with no
  # config.
  onSuccess = lib.optional (cfg.shadow != [ ] && !cfg.dryRun) "autoland-shadow@%i.service";
  service = {
    description = "autoland: Conary's flow beside the steward's pass of %I";
    restartIfChanged = false;
    # After a render under way, and starting none: it runs after each
    # pass, and a pass changes nothing the render reads.
    after = [ "network-online.target" ] ++ needs ++ render;
    wants = [ "network-online.target" ];
    requires = needs;
    unitConfig.ConditionPathExists = instanceConfig;
    environment = ledger.env // ledger.flow.env // {
      CONARY_TOKEN_COMMAND = tokenCommand;
    };
    serviceConfig = {
      Type = "oneshot";
      # The flow's controller where it has one (burrow.ledger.controllerUser).
      User = ledger.flow.user;
      ExecStart = "${ledger.package}/bin/cn flow compare %I --config ${instanceConfig}";
      # Well inside the next pass's: a compare is one read of each PR.
      TimeoutStartSec = "10min";
      Nice = 15;
      # The App's key, which the token command reads.
      LoadCredential = credentials;
    };
  };
}
