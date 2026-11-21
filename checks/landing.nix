# Who lands each repo (autoland/landing.nix, Conary#313). By default the
# steward lands every repo: no conaryd runs for landing and nothing is
# forwarded, but the steward's handlers and its decision read the record,
# so a repo conaryd held stays off the steward's hands. With repos conaryd
# may have, conaryd's flow runs as the flow's user on the events file
# burrow-landing writes, which Conary's own loader takes as rendered and
# refuses with a key it doesn't know; burrow-landing runs as that user too,
# on each change of the setting, the steward's release and each deploy;
# deliveries go on to conaryd's port, and each repo's hook sends what
# conaryd takes. Each repo conaryd may have reads its own flow config, the
# one its shadow reads, so a remi-shaped repo (its train's gate on runners
# started fresh for its jobs) and a Conary-shaped one (on GitHub-hosted
# runners) land through one conaryd, each on its own evidence, as does a
# private one whose trainFresh says its gate runs on the runners dispatch
# registers for it, and Conary's own loaders take the events file and each
# config. A trainFresh with no fresh runners is refused. A repo not shadowed,
# or with a train and no evidence, is refused. Being listed gives conaryd
# no repo: that's the live setting's (crates/burrow-landing's tests show
# the handover, and narrow the events file to the repos it gives).
{ self, pkgs, nixpkgs, system, ... }:
let
  lib = nixpkgs.lib;
  bare = modules: (self.nixosConfigurations.example.extendModules { inherit modules; }).config;
  # homelab's train with its evidence, which conaryd's train steps on.
  machine = modules: bare ([{ burrow.autoland.repos."sam/homelab".trainNegative = "check / negative"; }] ++ modules);
  failing = c: map (a: a.message) (lib.filter (a: !a.assertion) c.assertions);
  plain = machine [ ];
  on = machine [{
    burrow.autoland.shadow = [ "Sam/HomeLab" "sam/plain" ];
    burrow.autoland.repos."sam/plain".settleDelay = 2;
    burrow.landing.conaryd = [ "sam/homelab" ];
  }];
  # The flow with a controller of its own (burrow.ledger.controllerUser),
  # as S1 runs it, with and without a repo for conaryd: the ledger keeps
  # the controller to the flow's own units, so burrow-landing, which runs
  # as it, must be one (the flow's environment, its state).
  controlled = conaryd: machine [{
    burrow.ledger.controllerUser = "conary-flow";
    burrow.autoland.shadow = [ "sam/homelab" ];
    burrow.landing.conaryd = conaryd;
  }];
  controlledRuns = c: c.systemd.services.burrow-landing.serviceConfig.User == "conary-flow"
    && c.systemd.services.burrow-landing.environment.CONARY_DATA or null == "${c.burrow.stateDir}/flow";
  # The owner's door in a repo conaryd may have: the flow asks for a late
  # review through it (its config's review_ask), whether it runs as
  # burrow.user or as a controller of its own, which the door's broker
  # lets ask too.
  asked = controller: machine [{
    burrow.ledger.controllerUser = lib.mkIf controller "conary-flow";
    burrow.autoland.shadow = [ "sam/homelab" ];
    burrow.autoland.askAsOwner.repos = [ "Sam/HomeLab" ];
    burrow.landing.conaryd = [ "sam/homelab" ];
  }];
  askedFlow = asked false;
  controlledDoor = (asked true).burrow.autoland.askAsOwner.door;
  # remi and Conary as remi runs them: each with its train, remi's gate on
  # the machine's fresh runners, Conary's on GitHub's. And a private repo
  # dispatch discovers (burrow.dispatch.privateOwners), not in the fresh
  # runners' repos, whose gate runs on the runners it registers for it.
  train = require: { train = true; trainRequire = [ require ]; trainNegative = "check / negative"; };
  pair = machine [{
    burrow.dispatch.fresh = { enable = true; owner = "Sam"; repos = [ "Sam/Remi" ]; };
    burrow.autoland.shadow = [ "sam/remi" "sam/conary" "sam/vault" ];
    burrow.autoland.repos."sam/remi" = train "fresh / check";
    burrow.autoland.repos."sam/conary" = train "check / check";
    burrow.autoland.repos."Sam/Vault" = train "fresh / check" // { trainFresh = true; };
    burrow.landing.conaryd = [ "sam/remi" "sam/conary" "sam/vault" ];
  }];
  pairTemplate = pair.burrow.landing.eventsTemplate;
  # A repo's config as the build renders it: with no role-run plugin, what
  # the machine renders is the same.
  pairConfig = repo: "${pair.burrow.autoland.flow.base}/${repo}.json";
  door = askedFlow.burrow.autoland.askAsOwner.door;
  units = on.systemd.services;
  # There only while burrow-landing gives conaryd a repo.
  given = "${on.burrow.stateDir}/landing/conaryd-given";
  flow = units.conary-flow;
  driver = units.burrow-landing;
  template = on.burrow.landing.eventsTemplate;
  handlers = c: c.burrow.events.handlers;
  wrong = lib.filter (x: x != null) [
    (if plain.systemd.services ? conary-flow then "conaryd runs with no repo it may have" else null)
    (if !(plain.systemd.services ? burrow-landing) || plain.systemd.services.burrow-landing.serviceConfig.User != plain.burrow.ledger.flow.user then "with no repo for conaryd, nothing could hand back one the record still gives it" else null)
    (if plain.burrow.events.landing.forward != null then "deliveries are forwarded with no conaryd" else null)
    (if plain.burrow.events.landing.record != plain.burrow.landing.record then "burrow-events doesn't read the record by default" else null)
    (if !(handlers plain).autoland.landing || !(handlers plain).autoland-continue.landing then "the steward's handlers take repos the record gives conaryd" else null)
    (if lib.any (h: h.landing) (lib.attrValues (lib.removeAttrs (handlers plain) [ "autoland" "autoland-continue" ])) then "a handler other than the steward's is held to the record" else null)
    (if plain.burrow.settings.effective.landingRecord != plain.burrow.landing.record then "the steward's decision doesn't read the record" else null)
    (if plain.burrow.settings.effective.conaryd != [ ] then "a repo conaryd may have by default" else null)
    (if failing (controlled [ ]) != [ ] then "a machine with a flow controller and no repo for conaryd fails: ${toString (failing (controlled [ ]))}" else null)
    (if failing (controlled [ "sam/homelab" ]) != [ ] then "a machine with a flow controller and a repo for conaryd fails: ${toString (failing (controlled [ "sam/homelab" ]))}" else null)
    (if !(controlledRuns (controlled [ ])) || !(controlledRuns (controlled [ "sam/homelab" ])) then "burrow-landing doesn't run as the flow's controller on the flow's state" else null)
    (if failing on != [ ] then "a machine with a shadowed repo for conaryd fails: ${toString (failing on)}" else null)
    (if on.burrow.settings.effective.conaryd != [ "sam/homelab" ] then "the settings don't know which repos conaryd may have" else null)
    (if flow.serviceConfig.User != on.burrow.ledger.flow.user || driver.serviceConfig.User != on.burrow.ledger.flow.user then "conaryd's flow or burrow-landing runs as someone other than the flow's user" else null)
    (if flow.environment.CONARY_EVENTS_CONFIG != "${on.burrow.stateDir}/landing/conaryd-events.json" then "conaryd doesn't read the events file burrow-landing writes" else null)
    (if !(lib.elem given flow.unitConfig.ConditionPathExists) then "conaryd starts with no repo given it, and fails on credentials a machine that gives it none needn't have" else null)
    (if flow.serviceConfig.KillMode != "process" then "a restart of conaryd kills the flow's workers" else null)
    (if !(lib.elem "${on.burrow.stateDir}/autoland/landing-released.json" on.systemd.paths.burrow-landing.pathConfig.PathChanged) then "burrow-landing doesn't hear the steward let go" else null)
    (if !(lib.elem "${on.burrow.stateDir}/landing/conaryd-started" on.systemd.paths.burrow-landing.pathConfig.PathChanged) then "burrow-landing doesn't hear conaryd start again without a repo it let go of" else null)
    (if !(lib.hasInfix "conaryd-started" units.conary-flow-given.script) || on.systemd.paths.conary-flow-given.pathConfig.PathChanged != "${on.burrow.stateDir}/landing/conaryd-events.serial" then "conaryd's restart isn't recorded by publication, so a free lock would count as its release" else null)
    (if lib.toInt (toString (driver.unitConfig.StartLimitBurst or 5)) < 20 then "a handover's burst of starts can hit burrow-landing's start limit, and fail its path unit" else null)
    (if !(on.systemd.timers ? burrow-landing) || driver.serviceConfig.Restart != "on-failure" then "a failed or missed run of burrow-landing isn't run again" else null)
    (if !(flow.serviceConfig ? ExecStartPre) then "conaryd reads the hooks' secret untrimmed" else null)
    (if !(lib.elem on.burrow.settings.effective.overlay on.systemd.paths.burrow-landing.pathConfig.PathChanged) then "burrow-landing doesn't hear the setting change" else null)
    (if !(lib.elem on.burrow.updates.renovate.runLock on.systemd.paths.burrow-landing.pathConfig.PathChanged) then "burrow-landing doesn't hear a run of Renovate end" else null)
    (if on.systemd.tmpfiles.settings.burrow.${on.burrow.updates.renovate.runLock}.f.mode != "0644" then "Renovate's run lock may be unreadable to burrow-landing, which runs as the flow's user" else null)
    (if on.burrow.events.landing.forward != on.burrow.landing.port then "deliveries don't go where conaryd listens" else null)
    # Every event Conary's own table kicks a pass on (conary-github's
    # events::EVENTS, named in its docs/conaryd.md), workflow jobs' start
    # among them: one missing is a kick conaryd never gets.
    (let takes = [ "pull_request" "pull_request_review" "pull_request_review_comment" "pull_request_review_thread"
      "issue_comment" "check_suite" "check_run" "workflow_run" "status" "workflow_job" "push" ];
      missing = lib.filter (e: !(lib.elem e on.burrow.events.landing.events && lib.elem e on.burrow.events.hookEvents)) takes;
    in if missing != [ ] then "the hooks don't send what conaryd takes: ${toString missing}" else null)
    (if !(lib.hasPrefix "${on.burrow.stateDir}/" on.burrow.events.landing.outbox)
        || on.systemd.tmpfiles.settings.burrow.${on.burrow.events.landing.outbox}.d.user != "burrow-events"
        || !(lib.elem on.burrow.events.landing.outbox on.systemd.services.burrow-events.serviceConfig.ReadWritePaths)
      then "deliveries for conaryd aren't held where burrow-events can write and a reboot keeps" else null)
    (if plain.systemd.services.burrow-events.serviceConfig ? ReadWritePaths then "a machine forwarding nothing gives burrow-events somewhere to write" else null)
    (if !(lib.any (lib.hasInfix "shadow each repo first") (failing (machine [{ burrow.landing.conaryd = [ "sam/homelab" ]; }]))) then "a repo conaryd may have that was never shadowed is taken" else null)
    (if failing pair != [ ] then "two repos with different flow configs can't both be given to conaryd: ${toString (failing pair)}" else null)
    (if !(lib.any (lib.hasInfix "a train with no trainNegative") (failing (bare [{
      burrow.autoland.shadow = [ "sam/homelab" ];
      burrow.landing.conaryd = [ "sam/homelab" ];
    }]))) then "a repo whose train has no evidence, which conaryd's train can't step on, is given to it" else null)
    (if !(lib.all (m: lib.any (lib.hasInfix "trainFresh counts the machine's fresh runners") (failing (machine [
      { burrow.autoland.repos."sam/homelab".trainFresh = true; } m ]))) [
      { }
      { burrow.dispatch.enable = lib.mkForce false; burrow.dispatch.fresh = { enable = true; owner = "Sam"; repos = [ ]; }; }
    ]) then "a repo whose evidence is to count the machine's fresh runners is taken with none" else null)
    (if failing askedFlow != [ ] then "a machine whose flow asks through the owner's door fails: ${toString (failing askedFlow)}" else null)
    (if failing (asked true) != [ ] then "a machine whose flow, run by a controller of its own, asks through the owner's door fails: ${toString (failing (asked true))}" else null)
    (if !(lib.elem "conary-flow" controlledDoor.askers) || !(lib.elem askedFlow.burrow.user controlledDoor.askers) then "the owner's door's broker doesn't let both the steward and the flow's controller ask: ${toString controlledDoor.askers}" else null)
    (if askedFlow.burrow.autoland.askAsOwner.door.askers != [ askedFlow.burrow.user ] then "with no controller, the owner's door's broker lets someone other than burrow.user ask" else null)
  ];
  askedConfig = "${askedFlow.burrow.autoland.flow.base}/sam/homelab.json";
  controlledConfig = "${(asked true).burrow.autoland.flow.base}/sam/homelab.json";
  flowFiles = "${self.packages.${system}.burrow-flow-files}/bin/burrow-flow-files";
in pkgs.runCommand "landing-check" { nativeBuildInputs = [ pkgs.jq ]; } ''
  fail() { echo "$*"; exit 1; }
  ${lib.concatMapStrings (w: "fail ${lib.escapeShellArg "landing: ${w}"}\n") wrong}
  # Conary's own type for conaryd's events file takes the one rendered, and
  # refuses what conaryd would.
  ${flowFiles} events ${template} || fail "Conary's events::Runtime refuses the rendered events file"
  jq -e '.repos == ["sam/homelab"] and .listen == "127.0.0.1:${toString on.burrow.landing.port}"' ${template} >/dev/null \
    || fail "conaryd's events file isn't rendered for the repos it may have, or listens where nothing forwards: $(jq -c '[.repos, .listen]' ${template})"
  jq -e '.flow_config | keys == ["sam/homelab"]' ${template} >/dev/null \
    || fail "conaryd's events file gives flow configs to repos it may not have: $(jq -c .flow_config ${template})"
  # The live one, as the machine renders it, so a role set live is read at
  # conaryd's next pass (autoland/shadow.nix).
  [ "$(jq -r '.flow_config["sam/homelab"]' ${template})" = ${on.burrow.autoland.flowConfigs}/sam/homelab.json ] \
    || fail "conaryd's flow config isn't the repo's live one: $(jq -c .flow_config ${template})"
  # remi and Conary through one conaryd: the events file and each repo's
  # config, as Conary's loaders read them, each repo reading its own, with
  # its own evidence.
  ${flowFiles} events ${pairTemplate} || fail "Conary's events::Runtime refuses remi's and Conary's events file"
  ${flowFiles} flow ${pairConfig "sam/remi"} ${pairConfig "sam/conary"} ${pairConfig "sam/vault"} || fail "Conary's flow::Config refuses remi's, Conary's or vault's flow config"
  for repo in sam/remi sam/conary sam/vault; do
    [ "$(jq -r --arg r $repo '.flow_config[$r]' ${pairTemplate})" = ${pair.burrow.autoland.flowConfigs}/$repo.json ] \
      || fail "$repo doesn't read its own flow config"
  done
  jq -e '.evidence == {signer: "${pair.burrow.ledger.signerWorkflow}", fresh: "fresh-${pair.networking.hostName}-", require: ["fresh / check"], negative: "check / negative"}' ${pairConfig "sam/remi"} >/dev/null \
    || fail "remi's evidence doesn't count its fresh runners: $(jq -c .evidence ${pairConfig "sam/remi"})"
  jq -e '.evidence == {signer: "${pair.burrow.ledger.signerWorkflow}", fresh: null, require: ["check / check"], negative: "check / negative"}' ${pairConfig "sam/conary"} >/dev/null \
    || fail "Conary's evidence counts a runner other than GitHub's: $(jq -c .evidence ${pairConfig "sam/conary"})"
  jq -e '.evidence == {signer: "${pair.burrow.ledger.signerWorkflow}", fresh: "fresh-${pair.networking.hostName}-", require: ["fresh / check"], negative: "check / negative"}' ${pairConfig "sam/vault"} >/dev/null \
    || fail "a private repo's evidence doesn't count the runners dispatch registers for it: $(jq -c .evidence ${pairConfig "sam/vault"})"
  # Conary refuses an events file whose flow configs don't match its repos.
  jq '.flow_config |= del(.["sam/conary"])' ${pairTemplate} > missing.json
  jq '.repos = ["sam/remi"]' ${pairTemplate} > unlisted.json
  ! ${flowFiles} events missing.json || fail "an events file listing a repo with no flow config was taken"
  ! ${flowFiles} events unlisted.json || fail "an events file giving a flow config to a repo it doesn't list was taken"
  # conaryd's flow asks through the owner's door, in its repos and with
  # its command, and Conary's flow::Config takes that; with no door, the
  # key is left out, and the flow asks with its own comment.
  jq -e --argjson door '${builtins.toJSON { inherit (door) command repos; }}' '.review_ask == $door' ${askedConfig} >/dev/null \
    || fail "conaryd's flow doesn't ask through the owner's door: $(jq -c .review_ask ${askedConfig})"
  jq -e --argjson door '${builtins.toJSON { inherit (controlledDoor) command repos; }}' '.review_ask == $door' ${controlledConfig} >/dev/null \
    || fail "conaryd's flow, run by a controller of its own, doesn't ask through the owner's door: $(jq -c .review_ask ${controlledConfig})"
  ${flowFiles} flow ${askedConfig} || fail "Conary's flow::Config refuses the owner's door"
  jq -e 'has("review_ask") | not' ${on.burrow.autoland.flow.base}/sam/homelab.json >/dev/null \
    || fail "a flow config asks through a door the machine doesn't have"
  grep -q "${template}" ${lib.last (lib.splitString " " driver.serviceConfig.ExecStart)} || fail "burrow-landing doesn't write the rendered events file"
  jq -e '.renovateLock == "${on.burrow.updates.renovate.runLock}"' ${lib.last (lib.splitString " " driver.serviceConfig.ExecStart)} >/dev/null \
    || fail "burrow-landing hands a repo over while a run of Renovate that may merge there is under way"
  jq -e '.conaryd.given == "${given}"' ${lib.last (lib.splitString " " driver.serviceConfig.ExecStart)} >/dev/null \
    || fail "burrow-landing doesn't keep the marker conaryd starts on, so it starts with no repo given it"
  jq '. + {bogus: 1}' ${template} > key.json
  jq '.repos = "sam/homelab"' ${template} > type.json
  ! ${flowFiles} events key.json || fail "an events file with a key Conary doesn't know was taken"
  ! ${flowFiles} events type.json || fail "an events file with a value of the wrong type was taken"
  touch $out
''
