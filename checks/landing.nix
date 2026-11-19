# Who lands each repo (autoland/landing.nix, Conary#313). By default the
# steward lands every repo: no conaryd runs for landing and nothing is
# forwarded, but the steward's handlers and its decision read the record,
# so a repo conaryd held stays off the steward's hands. With repos conaryd
# may have, conaryd's flow runs as the flow's user on the events file
# burrow-landing writes, which Conary's own loader takes as rendered and
# refuses with a key it doesn't know; burrow-landing runs as that user too,
# on each change of the setting, the steward's release and each deploy;
# deliveries go on to conaryd's port, and each repo's hook sends what
# conaryd takes. A repo not shadowed, or two repos that render different
# flow configs, are refused. Being listed gives conaryd no repo: that's the
# live setting's (crates/burrow-landing's tests show the handover).
{ self, pkgs, nixpkgs, system, ... }:
let
  lib = nixpkgs.lib;
  machine = modules: (self.nixosConfigurations.example.extendModules { inherit modules; }).config;
  failing = c: map (a: a.message) (lib.filter (a: !a.assertion) c.assertions);
  plain = machine [ ];
  on = machine [{
    burrow.autoland.shadow = [ "Sam/HomeLab" "sam/plain" ];
    burrow.autoland.repos."sam/plain".settleDelay = 2;
    burrow.landing.conaryd = [ "sam/homelab" ];
  }];
  units = on.systemd.services;
  flow = units.conary-flow;
  driver = units.burrow-landing;
  template = on.burrow.landing.eventsTemplate;
  handlers = c: c.burrow.events.handlers;
  wrong = lib.filter (x: x != null) [
    (if plain.systemd.services ? conary-flow || plain.systemd.services ? burrow-landing then "conaryd runs with no repo it may have" else null)
    (if plain.burrow.events.landing.forward != null then "deliveries are forwarded with no conaryd" else null)
    (if plain.burrow.events.landing.record != plain.burrow.landing.record then "burrow-events doesn't read the record by default" else null)
    (if !(handlers plain).autoland.landing || !(handlers plain).autoland-continue.landing then "the steward's handlers take repos the record gives conaryd" else null)
    (if lib.any (h: h.landing) (lib.attrValues (lib.removeAttrs (handlers plain) [ "autoland" "autoland-continue" ])) then "a handler other than the steward's is held to the record" else null)
    (if plain.burrow.settings.effective.landingRecord != plain.burrow.landing.record then "the steward's decision doesn't read the record" else null)
    (if plain.burrow.settings.effective.conaryd != [ ] then "a repo conaryd may have by default" else null)
    (if failing on != [ ] then "a machine with a shadowed repo for conaryd fails: ${toString (failing on)}" else null)
    (if on.burrow.settings.effective.conaryd != [ "sam/homelab" ] then "the settings don't know which repos conaryd may have" else null)
    (if flow.serviceConfig.User != on.burrow.ledger.flow.user || driver.serviceConfig.User != on.burrow.ledger.flow.user then "conaryd's flow or burrow-landing runs as someone other than the flow's user" else null)
    (if flow.environment.CONARY_EVENTS_CONFIG != "${on.burrow.stateDir}/landing/conaryd-events.json" then "conaryd doesn't read the events file burrow-landing writes" else null)
    (if flow.serviceConfig.KillMode != "process" then "a restart of conaryd kills the flow's workers" else null)
    (if !(lib.elem "${on.burrow.stateDir}/autoland/landing-released.json" on.systemd.paths.burrow-landing.pathConfig.PathChanged) then "burrow-landing doesn't hear the steward let go" else null)
    (if !(lib.elem on.burrow.settings.effective.overlay on.systemd.paths.burrow-landing.pathConfig.PathChanged) then "burrow-landing doesn't hear the setting change" else null)
    (if on.systemd.paths.conary-flow-given.pathConfig.PathChanged != "${on.burrow.stateDir}/landing/conaryd-events.json" then "conaryd isn't restarted when it's given a repo" else null)
    (if on.burrow.events.landing.forward != on.burrow.landing.port then "deliveries don't go where conaryd listens" else null)
    (if !(lib.all (e: lib.elem e on.burrow.events.hookEvents) [ "check_run" "pull_request_review_thread" "push" ]) then "the hooks don't send what conaryd takes" else null)
    (if !(lib.any (lib.hasInfix "shadow each repo first") (failing (machine [{ burrow.landing.conaryd = [ "sam/homelab" ]; }]))) then "a repo conaryd may have that was never shadowed is taken" else null)
    (if !(lib.any (lib.hasInfix "render different flow configurations") (failing (machine [{
      burrow.autoland.shadow = [ "sam/homelab" "sam/plain" ];
      burrow.autoland.repos."sam/plain".settleDelay = 2;
      burrow.landing.conaryd = [ "sam/homelab" "sam/plain" ];
    }]))) then "two repos with different flow configs are both given to one conaryd" else null)
  ];
  flowFiles = "${self.packages.${system}.burrow-flow-files}/bin/burrow-flow-files";
in pkgs.runCommand "landing-check" { nativeBuildInputs = [ pkgs.jq ]; } ''
  fail() { echo "$*"; exit 1; }
  ${lib.concatMapStrings (w: "fail ${lib.escapeShellArg "landing: ${w}"}\n") wrong}
  # Conary's own type for conaryd's events file takes the one rendered, and
  # refuses what conaryd would.
  ${flowFiles} events ${template} || fail "Conary's events::Runtime refuses the rendered events file"
  jq -e '.repos == [] and .listen == "127.0.0.1:${toString on.burrow.landing.port}"' ${template} >/dev/null \
    || fail "conaryd is given a repo by the build, not the setting, or listens where nothing forwards: $(jq -c '[.repos, .listen]' ${template})"
  cmp "$(jq -r .flow_config ${template})" ${on.burrow.autoland.flowConfigs}/sam/homelab.json \
    || fail "conaryd's flow config isn't the repo's rendered one"
  grep -q "${template}" ${lib.last (lib.splitString " " driver.serviceConfig.ExecStart)} || fail "burrow-landing doesn't write the rendered events file"
  jq '. + {bogus: 1}' ${template} > key.json
  jq '.repos = "sam/homelab"' ${template} > type.json
  ! ${flowFiles} events key.json || fail "an events file with a key Conary doesn't know was taken"
  ! ${flowFiles} events type.json || fail "an events file with a value of the wrong type was taken"
  touch $out
''
