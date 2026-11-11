# The shadow (autoland/shadow.nix): a successful pass of a repo starts
# `autoland-shadow@` on it, a unit of its own, so its errors and timeout
# never touch the pass; the hourly pass starts none, and a machine with no
# shadowed repo has no such unit. It runs `cn flow compare` on the ledger's
# env and the steward's own token, only for a repo with a config, which is
# at its lowercased name, as burrow-events names a pass's instance. Each
# config holds exactly the keys Conary's flow::Config takes
# (crates/conary-github/src/flow/pass.rs, types.rs, evidence.rs), which
# refuses any other: the steward's settings for that repo, its overrides
# found whatever their case, in seconds, trusting its authors and its bots
# (the machinery's App only where there is one), with evidence only where
# the repo has a train negative.
{ self, pkgs, nixpkgs, ... }:
let
  lib = nixpkgs.lib;
  base = machinery: (self.nixosConfigurations.example.extendModules {
    modules = [{
      burrow.autoland.shadow = [ "Sam/HomeLab" "sam/plain" ];
      burrow.autoland.repos."sam/homelab".trainNegative = "negative";
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
  configs = lib.removeSuffix "/%I.json" shadow.unitConfig.ConditionPathExists;
  # The flow config of each machine's homelab and plain, as JSON.
  read = m: let s = m.systemd.services."autoland-shadow@"; dir = lib.removeSuffix "/%I.json" s.unitConfig.ConditionPathExists;
    in { homelab = "${dir}/sam/homelab.json"; plain = "${dir}/sam/plain.json"; };
  withApp = read machine;
  noApp = read (base false);
  none = (self.nixosConfigurations.example.extendModules { modules = [ ]; }).config.systemd.services;
  keys = {
    top = [ "evidence" "plugins" "settings" "trusted_authors" ];
    evidence = [ "fresh" "negative" "require" "signer" ];
    settings = [ "batch_label" "first_label" "handoff_after" "hold_label" "hold_stall_after"
      "job_login" "job_timeout" "owner_label" "people" "proof_slots" "proof_timeout" "proofs"
      "renovate_email" "renovate_login" "renovate_merge_after" "renovate_sweep" "require_ci"
      "review_authors" "reviewers" "rounds" "session_handoff_after" "settle_delay" "stall_after"
      "train" "train_stall_after" "verdict_authors" ];
  };
  # Every key a config has: all of Settings' but the seal keys,
  # judge_engines, bot_emails and renovate_run, left to Conary's defaults.
  wiring = {
    onSuccess = units."autoland@".unitConfig.OnSuccess == [ "autoland-shadow@%i.service" ];
    hourly = (units.autoland.unitConfig.OnSuccess or [ ]) == [ ];
    noPost = !(units."autoland@".serviceConfig ? ExecStartPost);
    exec = shadow.serviceConfig.ExecStart == "${machine.burrow.ledger.package}/bin/cn flow compare %I --config ${configs}/%I.json";
    timeout = shadow.serviceConfig.TimeoutStartSec == "10min";
    user = shadow.serviceConfig.User == machine.burrow.user;
    ledger = shadow.environment.CONARY_LEDGER == machine.burrow.ledger.dir;
    token = lib.hasSuffix "/bin/autoland-steward token" shadow.environment.CONARY_TOKEN_COMMAND;
    packageCarriesGit = !(lib.elem machine.programs.git.package shadow.path);
    key = lib.any (lib.hasPrefix "app-key:") shadow.serviceConfig.LoadCredential;
    unshadowed = !(none ? "autoland-shadow@") && (none."autoland@".unitConfig.OnSuccess or [ ]) == [ ];
  };
  wrong = lib.attrNames (lib.filterAttrs (_: ok: !ok) wiring);
in pkgs.runCommand "autoland-shadow-check" { nativeBuildInputs = [ pkgs.jq ]; } ''
  fail() { echo "$*"; exit 1; }
  ${lib.concatMapStrings (w: "fail 'the shadow unit is wrong: ${w}'\n") wrong}
  [ "$(cd ${configs} && find . -type l -o -type f | sort | tr '\n' ' ')" = "./sam/homelab.json ./sam/plain.json " ] \
    || fail "configs aren't each shadowed repo's, lowercased: $(cd ${configs} && find . -type l -o -type f)"
  for f in ${withApp.homelab} ${withApp.plain} ${noApp.plain}; do
    jq -e --argjson k '${builtins.toJSON keys}' '
      (keys == $k.top) and (.settings | keys == $k.settings)
      and (.evidence == null or (.evidence | keys == $k.evidence))' $f >/dev/null \
      || fail "$f has keys Conary's doesn't: $(jq -c '[keys, (.settings | keys)]' $f)"
  done
  jq -e '.evidence.negative == "negative" and .settings.train' ${withApp.homelab} >/dev/null \
    || fail "homelab's overrides (its train and negative) are lost to its name's case"
  jq -e '.evidence == null and (.settings.train | not)' ${withApp.plain} >/dev/null || fail "plain has evidence with no train negative"
  jq -e '.settings.settle_delay == 120 and .settings.reviewers == [] and .settings.proof_slots == 1
    and .settings.proofs == {gate: {Run: [.settings.proofs.gate.Run[0], "-c", "make check"]}, vm: {Build: "checks.x86_64-linux.vm"}}
    and .settings.review_authors == ["sams-steward[bot]"] and .settings.people == ["sam"]' \
    ${withApp.plain} >/dev/null || fail "plain's settings aren't the steward's: $(jq -c .settings ${withApp.plain})"
  jq -e '.trusted_authors == ["sam", "dependabot[bot]", "renovate[bot]", "sam-workers[bot]"]' ${withApp.plain} >/dev/null \
    || fail "with a machinery App, not the steward's authors and bots: $(jq -c .trusted_authors ${withApp.plain})"
  jq -e '.trusted_authors == ["sam", "dependabot[bot]", "renovate[bot]"] and .settings.people == ["sam"]' ${noApp.plain} >/dev/null \
    || fail "without one, not the steward's authors and bots: $(jq -c .trusted_authors ${noApp.plain})"
  touch $out
''
