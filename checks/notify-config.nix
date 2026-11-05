# HOST-notify as the example machine has it: the config built into it
# reads (a field the binary doesn't know fails it), and `pending`
# answers without GitHub. Its rules are crates/burrow-notify's tests.
{ self, pkgs, ... }:
let
  notify = pkgs.lib.getExe self.nixosConfigurations.example.config.burrow.tools.notify;
in pkgs.runCommand "notify-config-check" { } ''
  code=0
  ${notify} pending nothing-asked || code=$?
  [ "$code" = 1 ] || { echo "pending: exit $code"; exit 1; }
  touch $out
''
