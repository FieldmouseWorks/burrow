# HOST-escalation as the example machine has it: the config built into it
# reads (a field the binary doesn't know fails it), and `route` sends a
# one-way door to the owner and anything else to the operator, without
# GitHub. Its rules are crates/burrow-escalation's tests.
{ self, pkgs, ... }:
let
  escalation = pkgs.lib.getExe self.nixosConfigurations.example.config.burrow.tools.escalation;
in
pkgs.runCommand "escalation-config-check" { nativeBuildInputs = [ pkgs.jq ]; } ''
  ${escalation} route confirm-access "the Renovate App's settings" >owner.json
  jq -e '.route == "owner" and .reason.kind == "confirm-access"' owner.json
  ${escalation} route hold "what wend#218's hold waits for" >operator.json
  jq -e '.route == "operator"' operator.json
  code=0
  ${escalation} shout 2>/dev/null || code=$?
  [ "$code" = 2 ] || { echo "a bad word: exit $code"; exit 1; }
  touch $out
''
