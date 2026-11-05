# The example's stall watchdog reads the config its module writes (a
# field the binary doesn't know fails it).
{ self, pkgs, ... }:
pkgs.runCommand "watchdog-config-check" { } ''
  ${self.nixosConfigurations.example.config.systemd.services.burrow-watchdog.serviceConfig.ExecStart} status
  touch $out
''
