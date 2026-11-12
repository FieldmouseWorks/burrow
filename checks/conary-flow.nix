# CONARY_FLOW_CONFIG is rendered from the steward's settings, never written by
# hand. For the example machine, the generated config and policy pass the
# Rust test helper's validation (it never reaches GitHub), and
# the config carries the steward's authors and reviewers. Setting the variable
# by hand beside the generated one fails the build.
#
#   nix build .#checks.x86_64-linux.conary-flow -L
{ self, pkgs, nixpkgs, ... }:
let
  lib = nixpkgs.lib;
  example = self.nixosConfigurations.example;
  env = example.config.burrow.ledger.env;
  cn = self.packages.${pkgs.stdenv.hostPlatform.system}.conary-flow-check;
  withHand = example.extendModules {
    modules = [{ environment.variables.CONARY_FLOW_CONFIG = "/etc/hand-written.json"; }];
  };
  handWritten = (builtins.tryEval (builtins.deepSeq withHand.config.environment.variables true)).success;
in pkgs.runCommand "conary-flow-check" { nativeBuildInputs = [ cn pkgs.jq ]; } ''
  [ ${lib.boolToString handWritten} = false ] || { echo "a hand-written flow config was accepted beside the generated one"; exit 1; }
  conary-flow-check flow compare --config ${env.CONARY_FLOW_CONFIG} --policy ${env.CONARY_POLICY} sam/homelab
  jq -e '.trusted_authors == ["sam"] and .settings.review_authors == ["sams-steward[bot]"]' ${env.CONARY_FLOW_CONFIG}
  # The check fails on what it is for: a config with a key of its own.
  jq '. + {extra: 1}' ${env.CONARY_FLOW_CONFIG} >bad.json
  if conary-flow-check flow compare --config bad.json --policy ${env.CONARY_POLICY} sam/homelab; then
    echo "a config with an unknown key was accepted"; exit 1
  fi
  echo '{' >broken.json
  if conary-flow-check flow compare --config broken.json --policy ${env.CONARY_POLICY} sam/homelab; then
    echo "malformed JSON was accepted"; exit 1
  fi
  if conary-flow-check flow compare --config missing.json --policy ${env.CONARY_POLICY} sam/homelab; then
    echo "an unreadable config was accepted"; exit 1
  fi
  touch $out
''
