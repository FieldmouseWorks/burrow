# The example's stall watchdog reads the config its module writes (a
# field the binary doesn't know fails it).
{ self, pkgs, ... }:
pkgs.runCommand "watchdog-config-check" { } ''
  ${self.nixosConfigurations.example.config.systemd.services.burrow-watchdog.serviceConfig.ExecStart} status
  env ${self.nixosConfigurations.example.config.systemd.services.burrow-degradations.serviceConfig.Environment} \
    ${self.nixosConfigurations.example.config.systemd.services.burrow-degradations.serviceConfig.ExecStart} --check
  touch $out
''
