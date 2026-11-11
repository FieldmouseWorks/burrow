# The shadow (autoland/shadow.nix): a pass of a shadowed repo ends with
# `cn flow compare` on it, on the ledger's env and the steward's own token,
# its lines marked `shadow:` in the pass's journal, whatever case systemd's
# %I gives the repo in; a pass of any other repo runs nothing, and the
# hourly pass has no shadow at all. Each repo's flow config holds exactly
# the keys Conary's flow::Config takes (crates/conary-github/src/flow/pass.rs,
# types.rs, evidence.rs), which refuses any other, with the steward's
# settings for that repo in seconds; a repo with no train negative has no
# evidence section.
{ self, pkgs, nixpkgs, ... }:
let
  lib = nixpkgs.lib;
  # A `cn` that says what it was asked, with what env, and the config it
  # was handed.
  cn = pkgs.writeShellScriptBin "cn" ''
    [ "$1" = policy ] && exit 0
    echo "args: $*"
    echo "ledger: ''${CONARY_LEDGER:-}"
    echo "token: ''${CONARY_TOKEN_COMMAND:-}"
    while [ $# -gt 0 ]; do [ "$1" = --config ] && cp "$2" "$out_config"; shift; done
  '';
  machine = (self.nixosConfigurations.example.extendModules {
    modules = [{
      burrow.ledger.package = lib.mkForce cn;
      # The steward's tools stand in too: only its `token` is the shadow's,
      # and the real proj takes Codex's build with it.
      burrow.tools.proj = lib.mkForce (pkgs.writeShellScriptBin "proj" "exit 1");
      burrow.autoland.shadow = [ "sam/homelab" "sam/plain" ];
      burrow.autoland.repos."sam/homelab".trainNegative = "negative";
      burrow.autoland.repos."sam/plain" = {
        settleDelay = 2; codex = false;
        proofRuns.gate = "make check"; proofBuilds.vm = "checks.x86_64-linux.vm";
      };
    }];
  }).config;
  units = machine.systemd.services;
  post = units."autoland@".serviceConfig.ExecStartPost or [ ];
  script = lib.removeSuffix " %I" (lib.removePrefix "-" (lib.head post));
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
in pkgs.runCommand "autoland-shadow-check" { nativeBuildInputs = [ pkgs.jq ]; } ''
  fail() { echo "$*"; exit 1; }
  [ ${toString (builtins.length post)} = 1 ] || fail "autoland@ runs ${toString (builtins.length post)} shadows after a pass, not one"
  ${lib.optionalString ((units.autoland.serviceConfig.ExecStartPost or [ ]) != [ ]) "fail 'the hourly pass runs a shadow'"}
  case '${lib.head post}' in -*) ;; *) fail "a shadow that fails would fail the pass" ;; esac

  out_config=$PWD/other.json ${script} sam/elsewhere >other
  [ ! -s other ] && [ ! -e other.json ] || fail "a repo that isn't shadowed ran cn: $(cat other)"

  out_config=$PWD/homelab.json ${script} Sam/HomeLab >homelab
  grep -qx 'shadow: args: flow compare Sam/HomeLab --config .*/sam__homelab.json' homelab || fail "not cn flow compare: $(cat homelab)"
  grep -qx 'shadow: ledger: ${machine.burrow.ledger.dir}' homelab || fail "not on the ledger: $(cat homelab)"
  grep -qx 'shadow: token: .*/bin/autoland-steward token' homelab || fail "not the steward's token: $(cat homelab)"

  out_config=$PWD/plain.json ${script} sam/plain >plain
  for f in homelab plain; do
    jq -e --argjson k '${builtins.toJSON keys}' '
      (keys == $k.top) and (.settings | keys == $k.settings)
      and (.evidence == null or (.evidence | keys == $k.evidence))' $f.json >/dev/null \
      || fail "$f's flow config has keys Conary's doesn't: $(jq -c '[keys, (.settings | keys)]' $f.json)"
  done
  jq -e '.evidence != null and .settings.train' homelab.json >/dev/null || fail "homelab's train and its evidence are missing"
  jq -e '.evidence == null and (.settings.train | not)' plain.json >/dev/null || fail "plain has evidence with no train negative"
  jq -e '.settings.settle_delay == 120 and .settings.reviewers == []
    and .settings.proofs == {gate: {Run: [.settings.proofs.gate.Run[0], "-c", "make check"]}, vm: {Build: "checks.x86_64-linux.vm"}}
    and .settings.review_authors == ["sams-steward[bot]"] and .trusted_authors == ["sam"] and .settings.people == ["sam"]' \
    plain.json >/dev/null || fail "plain's settings aren't the steward's: $(jq -c .settings plain.json)"
  touch $out
''
