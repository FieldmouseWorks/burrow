# CONARY_FLOW_CONFIG is rendered from the steward's settings, never written by
# hand. For the example machine, the generated config and policy pass the
# validation `cn flow compare` runs first (dry: it never reaches GitHub), and
# the config carries the steward's authors and reviewers. Setting the variable
# by hand beside the generated one fails the build.
#
#   nix build .#checks.x86_64-linux.conary-flow -L
{ self, pkgs, nixpkgs, ... }:
let
  lib = nixpkgs.lib;
  example = self.nixosConfigurations.example;
  env = example.config.burrow.ledger.env;
  # What cn flow compare refuses before it reads GitHub: a config with a key
  # Conary doesn't know (its types deny unknown fields), a missing evidence
  # block or empty trust. The stand-in stops there, as a dry run does.
  cn = pkgs.writeShellScriptBin "cn" ''
    [ "$1 $2" = "flow compare" ] || exit 1
    shift 2
    while [ $# -gt 0 ]; do
      case $1 in
        --config) config=$2; shift 2 ;;
        --policy) policy=$2; shift 2 ;;
        *) shift ;;
      esac
    done
    ${pkgs.jq}/bin/jq -e '
      (keys - ["settings","trusted_authors","plugins","evidence"] == [])
      and (.settings | keys - ["people","reviewers","review_authors"] == [])
      and (.plugins | keys - ["review","fix","judge","proof"] == [])
      and (.evidence | keys == ["fresh","negative","require","signer"])
      and (.trusted_authors | length > 0) and (.settings.people | length > 0)
      and (.evidence.require | length > 0)' "$config" >/dev/null
    ${pkgs.jq}/bin/jq -e 'has("r2") and has("docs")' "$policy" >/dev/null
  '';
  withHand = example.extendModules {
    modules = [{ environment.variables.CONARY_FLOW_CONFIG = "/etc/hand-written.json"; }];
  };
  handWritten = (builtins.tryEval (builtins.deepSeq withHand.config.environment.variables true)).success;
in pkgs.runCommand "conary-flow-check" { nativeBuildInputs = [ cn pkgs.jq ]; } ''
  [ ${lib.boolToString handWritten} = false ] || { echo "a hand-written flow config was accepted beside the generated one"; exit 1; }
  cn flow compare --config ${env.CONARY_FLOW_CONFIG} --policy ${env.CONARY_POLICY} sam/homelab
  jq -e '.trusted_authors == ["sam"] and .settings.review_authors == ["sams-steward[bot]"]' ${env.CONARY_FLOW_CONFIG}
  # The check fails on what it is for: a config with a key of its own.
  jq '. + {extra: 1}' ${env.CONARY_FLOW_CONFIG} >bad.json
  ! cn flow compare --config bad.json --policy ${env.CONARY_POLICY} sam/homelab
  touch $out
''
