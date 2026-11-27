# The shadow (autoland/shadow.nix): a successful pass of a repo starts
# `autoland-shadow@` on it, a unit of its own, so its errors and timeout
# never touch the pass; the hourly pass starts none, and a machine with no
# shadowed repo has no such unit. It runs `cn flow compare` on the ledger's
# env and the steward's own token, only for a repo with a config, which is
# at its lowercased name, as burrow-events names a pass's instance. Each
# config holds every setting the pinned Conary's schema lists but those
# left to its defaults (`defaulted`, each with why), and Conary refuses any
# other: the steward's settings for that repo, its overrides
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
  };
  # The settings burrow leaves to Conary's defaults, each with why. A config
  # has every other key of Settings, as the pinned Conary's own schema
  # (`burrow-flow-files schema`) lists them, not a list written here: a pin
  # follow that adds a setting fails this check until burrow renders it or
  # says here why its default is burrow's too. Conary's defaults name no
  # machine (Conary#381), so the engines a judge is owed for and the bots'
  # addresses are the steward's own.
  defaulted = {
    default_priority = "the steward's one priority is its first label: an unlabelled PR is at Conary's medium";
    deferred = "the steward defers no check until review is ready, and Conary's default defers none";
    disputed_p2 = "undecided: the steward has no rule for a re-raised P2 or P3, and Conary's block holds it as any finding (burrow#1602)";
    fix_growth = "the steward hands no PR over for what its fix rounds add, nor does Conary's default";
    judge_key = "a seal key is named by its file, never written in the config";
    major_key = "a seal key is named by its file, never written in the config";
    judge_key_file = "only a machine with the judge's seal has one, and the example has none";
    renovate_run = "undecided: the steward waits Renovate's own timeout (renovateRun), the flow Conary's default (burrow#1602)";
  };
  wiring = {
    # The reviewer's protocol, as `cn` is told it, on every writer of the
    # ledger: the shadow's here, the steward's and conaryd's from the same
    # env. Without it `cn` reads the connector's posts as Conary's markers.
    reviewAdapter = machine.burrow.ledger.env.CONARY_REVIEW_ADAPTER or null == "codex"
      && shadow.environment.CONARY_REVIEW_ADAPTER or null == "codex";
    # The steward's copy of the red-fix branch is held to the option until
    # its Python goes (burrow#1310).
    redFixHeld = let r = builtins.tryEval (lib.any (lib.hasInfix "burrow#1310")
      (refused [{ burrow.autoland.redFixBranch = "fix/red-"; }])); in r.success && r.value;
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
    token = lib.hasSuffix "/bin/autoland-steward token" shadow.environment.CONARY_TOKEN_COMMAND;
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
  ${flowFiles} schema > schema.json || fail "burrow-flow-files can't print the pinned Conary's flow schema"
  jq -e --argjson d '${builtins.toJSON (lib.attrNames defaulted)}' \
    '(."$defs".Settings.properties | keys) as $s | ($d - $s) == [] and ($s | length) > ($d | length)' schema.json >/dev/null \
    || fail "a setting left to Conary's default isn't one of the pin's: $(jq -c '."$defs".Settings.properties | keys' schema.json)"
  jq -c --argjson d '${builtins.toJSON (lib.attrNames defaulted)}' \
    '{settings: ((."$defs".Settings.properties | keys) - $d), stages: (."$defs".StageLabels.properties | keys)}' schema.json > pinned.json
  for f in ${withApp.homelab} ${withApp.plain} ${noApp.plain}; do
    jq -e --argjson k '${builtins.toJSON keys}' --slurpfile p pinned.json '
      (keys == $k.top) and (.settings | keys == $p[0].settings)
      and (.settings.stage_labels | keys == $p[0].stages)
      and (.evidence == null or (.evidence | keys == $k.evidence))' $f >/dev/null \
      || fail "$f's keys aren't the pinned Conary's, but for those left to its defaults: $(jq -c --slurpfile p pinned.json '{top: keys, missing: ($p[0].settings - (.settings | keys)), extra: ((.settings | keys) - $p[0].settings)}' $f)"
    # The settings the pin's defaults changed, at what the old code did:
    # the connector's protocol, and the steward's red-fix branch.
    jq -e '.settings.review_adapter == "codex" and .settings.red_fix_branch == "autoland/red-"' $f >/dev/null \
      || fail "$f's reviewer protocol or red-fix branch isn't burrow's: $(jq -c '.settings | {review_adapter, red_fix_branch}' $f)"
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
