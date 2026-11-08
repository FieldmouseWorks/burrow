# HOST-health as the example machine builds it (crates/burrow-health,
# whose tests are the rest): it reads its config, and with no checks
# and no systemd in the sandbox, reports the required proj check missing.
{ self, pkgs, ... }:
let
  health = pkgs.lib.getExe self.nixosConfigurations.example.config.burrow.tools.health;
in pkgs.runCommand "health-config-check" { } ''
  got=$(${health} 2>err) || { echo "HOST-health failed (exit $?): $(cat err)"; exit 1; }
  [ "$got" = "check:proj-config fail" ] || { echo "HOST-health reported: $got"; exit 1; }
  touch $out
''
