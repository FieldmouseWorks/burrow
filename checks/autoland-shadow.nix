# The shadow (autoland/shadow.nix): a successful pass of a repo starts
# `autoland-shadow@` on it, a unit of its own, so its errors and timeout
# never touch the pass; the hourly pass starts none, and a machine with no
# shadowed repo has no such unit. It runs `cn flow compare` on the ledger's
# env and the App's token (burrow-upkeep's), only for a repo with a config, which is
# at its lowercased name, as burrow-events names a pass's instance. Each
# config holds exactly the keys Conary's flow::Config takes
# (crates/conary-github/src/flow/pass.rs, types.rs, evidence.rs), which
# refuses any other: the steward's settings for that repo, its overrides
# found whatever their case, in seconds, trusting its authors and its bots
# (the machinery's App only where there is one), with evidence only where
# the repo has a train negative. The machine's cn loads each as it's built
# (here the stand-in, which takes any), so each, and the machine's class
# policy, is read here by Conary's own loaders (crates/burrow-flow-files),
# which refuse a key Conary doesn't know or a value of the wrong type. A
# flow config or policy set by hand beside the rendered ones is refused.
{ self, pkgs, nixpkgs, system, ... }:
let
  lib = nixpkgs.lib;
  base = machinery: (self.nixosConfigurations.example.extendModules {
    modules = [{
      burrow.autoland.shadow = [ "Sam/HomeLab" "sam/plain" ];
      burrow.autoland.repos."sam/homelab" = { trainNegative = "negative"; trainSize = 3; };
      burrow.autoland.repos."sam/plain" = {
        settleDelay = 2; codex = false; proofSlots = 1;
        proofRuns.gate = "make check"; proofBuilds.vm = "checks.x86_64-linux.vm";
      };
      burrow.github.machinery = lib.mkIf (!machinery) (lib.mkForce null);
    }];
  }).config;
  machine = base true;
  units = machine.systemd.services;
  shadow = units."autoland-shadow@";
  # The unit reads the live configs, which the machine renders from the
  # base the build renders (autoland/shadow.nix): with no role-run plugin,
  # the same.
  live = lib.removeSuffix "/%I.json" shadow.unitConfig.ConditionPathExists;
  configs = machine.burrow.autoland.flow.base;
  # The flow config of each machine's homelab and plain, as JSON.
  read = m: let dir = m.burrow.autoland.flow.base;
    in { homelab = "${dir}/sam/homelab.json"; plain = "${dir}/sam/plain.json"; };
  withApp = read machine;
  noApp = read (base false);
  none = (self.nixosConfigurations.example.extendModules { modules = [ ]; }).config.systemd.services;
  # With a controller of its own (burrow.ledger.controllerUser), the flow
  # runs as it, on state of its own.
  controlled = (self.nixosConfigurations.example.extendModules {
    modules = [{
      burrow.autoland.shadow = [ "sam/plain" ];
      burrow.ledger.controllerUser = "conary-flow";
    }];
  }).config;
  controlledShadow = controlled.systemd.services."autoland-shadow@";
  # Someone else's user as the controller is refused, and so is a
  # producer's wiring with no controller at all.
  taken = (self.nixosConfigurations.example.extendModules {
    modules = [{
      burrow.autoland.shadow = [ "sam/plain" ];
      burrow.ledger.controllerUser = "burrow-events";
    }];
  }).config;
  failed = map (a: a.message) (lib.filter (a: !a.assertion) taken.assertions);
  uncontrolled = builtins.tryEval (builtins.deepSeq (machine.burrow.ledger.flow.producer "x") true);
  keys = {
    top = [ "evidence" "plugins" "settings" "trusted_authors" ];
    evidence = [ "fresh" "negative" "require" "signer" ];
    settings = [ "bot_emails" "flow_authors" "handoff_after" "hold_label" "hold_stall_after"
      "job_login" "job_timeout" "judge_engines" "major_key_file" "owner_label" "people" "priority_labels" "proof_slots" "proof_timeout" "proofs"
      "renovate_email" "renovate_login" "renovate_sweep" "require_ci"
      "review_authors" "reviewers" "rounds" "session_handoff_after" "settle_delay" "stall_after"
      "train" "train_size" "train_stall_after" "verdict_authors" ];
  };
  # Every key a config has: all of Settings' but the seal keys (the
  # example runs renovate-sweep, whose key the config names by its file),
  # renovate_run and default_priority, left to Conary's defaults. Conary's defaults name no
  # machine (Conary#381), so the engines a judge is owed for and the bots'
  # addresses are the steward's own.
  wiring = {
    onSuccess = units."autoland@".unitConfig.OnSuccess == [ "autoland-shadow@%i.service" ];
    hourly = (units.autoland.unitConfig.OnSuccess or [ ]) == [ ];
    noPost = !(units."autoland@".serviceConfig ? ExecStartPost);
    exec = shadow.serviceConfig.ExecStart == "${machine.burrow.ledger.package}/bin/cn flow compare %I --config ${live}/%I.json"
      && live == machine.burrow.autoland.flowConfigs;
    timeout = shadow.serviceConfig.TimeoutStartSec == "10min";
    user = shadow.serviceConfig.User == machine.burrow.user && !(shadow.environment ? CONARY_DATA);
    controlledHolds = lib.all (a: a.assertion) controlled.assertions;
    taken = lib.any (lib.hasInfix "burrow-events is already someone else's") failed;
    producerNeedsController = !uncontrolled.success && controlled.burrow.ledger.flow.controller == "conary-flow"
      && machine.burrow.ledger.flow.controller == null;
    controller = controlledShadow.serviceConfig.User == "conary-flow"
      && controlledShadow.environment.CONARY_DATA == "${controlled.burrow.stateDir}/flow";
    ledger = shadow.environment.CONARY_LEDGER == machine.burrow.ledger.dir;
    # Conary's token command, the shadow's and conaryd's alike: burrow-upkeep's.
    token = let t = shadow.environment.CONARY_TOKEN_COMMAND; in
      t == machine.burrow.autoland.flow.token
      && builtins.match "/nix/store/[^ ]+/bin/burrow-upkeep /nix/store/[^ ]+-burrow-upkeep[.]json token --bare" t != null;
    packageCarriesGit = !(lib.elem machine.programs.git.package shadow.path);
    key = lib.any (lib.hasPrefix "app-key:") shadow.serviceConfig.LoadCredential;
    unshadowed = !(none ? "autoland-shadow@") && (none."autoland@".unitConfig.OnSuccess or [ ]) == [ ];
  };
  wrong = lib.attrNames (lib.filterAttrs (_: ok: !ok) wiring);
  # A flow config or policy beside burrow's, in the machine's environment or
  # a unit's, fails the build; the rendered ones a unit carries don't.
  refused = modules: map (a: a.message) (lib.filter (a: !a.assertion)
    (self.nixosConfigurations.example.extendModules { inherit modules; }).config.assertions);
  says = what: lib.any (m: lib.hasInfix what m && lib.hasInfix "set by hand" m);
  handWritten = {
    renderedPasses = !(lib.any (lib.hasInfix "set by hand") (map (a: a.message) (lib.filter (a: !a.assertion) machine.assertions)));
    flowVariable = says "environment.variables.CONARY_FLOW_CONFIG"
      (refused [{ environment.variables.CONARY_FLOW_CONFIG = "/etc/conary/flow.json"; }]);
    flowUnit = says "systemd.services.autoland-shadow@.environment.CONARY_FLOW_CONFIG" (refused [{
      burrow.autoland.shadow = [ "sam/plain" ];
      systemd.services."autoland-shadow@".environment.CONARY_FLOW_CONFIG = "/etc/conary/flow.json";
    }]);
    # A unit handed a repo's rendered config, as the flow's own may be.
    renderedFlowPasses = !(lib.any (lib.hasInfix "set by hand") (refused [({ config, ... }: {
      burrow.autoland.shadow = [ "sam/plain" ];
      systemd.services.conary-flow.environment.CONARY_FLOW_CONFIG = "${config.burrow.autoland.flowConfigs}/sam/plain.json";
    })]));
    policyUnit = says "systemd.services.conary-mine.environment.CONARY_POLICY"
      (refused [{ systemd.services.conary-mine.environment.CONARY_POLICY = "/etc/conary/policy.json"; }]);
  };
  unrefused = lib.attrNames (lib.filterAttrs (_: ok: !ok) handWritten);
  flowFiles = "${self.packages.${system}.burrow-flow-files}/bin/burrow-flow-files";
  policy = machine.burrow.ledger.env.CONARY_POLICY;
in pkgs.runCommand "autoland-shadow-check" { nativeBuildInputs = [ pkgs.jq ]; } ''
  fail() { echo "$*"; exit 1; }
  ${lib.concatMapStrings (w: "fail 'the shadow unit is wrong: ${w}'\n") wrong}
  ${lib.concatMapStrings (w: "fail 'a Conary file set by hand beside the rendered ones is not refused: ${w}'\n") unrefused}
  [ "$(cd ${configs} && find . -type l -o -type f | sort | tr '\n' ' ')" = "./sam/homelab.json ./sam/plain.json " ] \
    || fail "configs aren't each shadowed repo's, lowercased: $(cd ${configs} && find . -type l -o -type f)"
  for f in ${withApp.homelab} ${withApp.plain} ${noApp.plain}; do
    jq -e --argjson k '${builtins.toJSON keys}' '
      (keys == $k.top) and (.settings | keys == $k.settings)
      and (.evidence == null or (.evidence | keys == $k.evidence))' $f >/dev/null \
      || fail "$f has keys Conary's doesn't: $(jq -c '[keys, (.settings | keys)]' $f)"
  done
  jq -e '.evidence.negative == "negative" and .settings.train and .settings.train_size == 3' ${withApp.homelab} >/dev/null \
    || fail "homelab's overrides (its train, negative and train size) are lost to its name's case"
  jq -e '.evidence == null and (.settings.train | not)' ${withApp.plain} >/dev/null || fail "plain has evidence with no train negative"
  jq -e '.settings.settle_delay == 120 and .settings.reviewers == [] and .settings.proof_slots == 1
    and .settings.proofs == {gate: {Run: [.settings.proofs.gate.Run[0], "-c", "make check"]}, vm: {Build: "checks.x86_64-linux.vm"}}
    and .settings.review_authors == ["chatgpt-codex-connector[bot]"] and .settings.people == ["sam"]
    and .settings.flow_authors == [.settings.verdict_authors[0]]
    and (.settings.flow_authors[0] | endswith("[bot]"))
    and .settings.judge_engines == []
    and .settings.priority_labels == {first: "high"}
    and .settings.stall_after == {high: 1800, medium: 7200, low: 7200}
    and .settings.bot_emails == ["49699333+dependabot[bot]@users.noreply.github.com"]' \
    ${withApp.plain} >/dev/null || fail "plain's settings aren't the steward's: $(jq -c .settings ${withApp.plain})"
  jq -e '.trusted_authors == ["sam", "dependabot[bot]", "renovate[bot]", "sam-workers[bot]"]
    and .settings.verdict_authors == ["sams-steward[bot]", "sam-workers[bot]"]' ${withApp.plain} >/dev/null \
    || fail "with a machinery App, not the steward's authors, bots and verdict authors: $(jq -c '{trusted_authors, verdict_authors: .settings.verdict_authors}' ${withApp.plain})"
  jq -e '.trusted_authors == ["sam", "dependabot[bot]", "renovate[bot]"] and .settings.people == ["sam"]
    and .settings.verdict_authors == ["sams-steward[bot]", "sam"]' ${noApp.plain} >/dev/null \
    || fail "without one, not the steward's authors, bots and verdict authors: $(jq -c '{trusted_authors, verdict_authors: .settings.verdict_authors}' ${noApp.plain})"
  # Conary's own types: the configs rendered, and the class policy. The
  # loader reads each key a config names by its file (renovate-sweep's, as
  # the example runs it), which only the machine has: a stand-in here.
  for f in ${withApp.homelab} ${withApp.plain} ${noApp.plain}; do
    jq -e '.settings.major_key_file == "/run/autoland-flow-keys/major-seal"' $f >/dev/null \
      || fail "$f doesn't name renovate-sweep's key by its file"
  done
  ${flowFiles} flow --stand-in-keys ${withApp.homelab} ${withApp.plain} ${noApp.plain} || fail "Conary's flow::Config refuses a rendered config"
  ${flowFiles} policy ${policy} || fail "Conary's Policy refuses the machine's class policy"
  # And they refuse what the machine's cn would: a key Conary doesn't know,
  # a value of the wrong type, a policy with a key it doesn't take.
  jq '.settings.bogus = 1' ${withApp.homelab} > key.json
  jq '.settings.rounds = "two"' ${withApp.homelab} > type.json
  jq '. + {bogus: []}' ${policy} > policy.json
  ! ${flowFiles} flow --stand-in-keys key.json || fail "a flow config with a key Conary doesn't know was taken"
  ! ${flowFiles} flow --stand-in-keys type.json || fail "a flow config with a value of the wrong type was taken"
  ! ${flowFiles} policy policy.json || fail "a policy with a key Conary doesn't know was taken"
  touch $out
''
