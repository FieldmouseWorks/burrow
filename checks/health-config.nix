# HOST-health as the example machine builds it (crates/burrow-health,
# whose tests are the rest): it reads its config, and with no checks
# and no systemd in the sandbox, reports the required proj check missing.
{ self, pkgs, ... }:
let
  before = self.nixosConfigurations.example.config;
  after = (self.nixosConfigurations.example.extendModules {
    modules = [{
      networking.hostName = pkgs.lib.mkForce "after";
      burrow.prefix = "changed";
      burrow.updates.enable = pkgs.lib.mkForce false;
      burrow.updates.requiredHealth = [ "target-required" ];
    }];
  }).config;
  health = before.environment.etc."burrow/health".source;
  targetHealth = after.environment.etc."burrow/health".source;
in
assert health == pkgs.lib.getExe before.burrow.tools.health;
assert targetHealth == pkgs.lib.getExe after.burrow.tools.health;
assert !(after.burrow.updates.enable);
pkgs.runCommand "health-config-check" { } ''
  got=$(${health} 2>err) || { echo "HOST-health failed (exit $?): $(cat err)"; exit 1; }
  [ "$got" = "check:proj-config fail" ] || { echo "HOST-health reported: $got"; exit 1; }
  [ -x ${targetHealth} ]
  ${targetHealth} > target 2>err
  grep -qx 'check:target-required fail' target
  touch $out
''
