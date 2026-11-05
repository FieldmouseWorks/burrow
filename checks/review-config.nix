# The example's reviewers' service reads the config its module writes
# (a field the binary doesn't know fails it).
{ self, pkgs, ... }:
pkgs.runCommand "review-config-check" { } ''
  ${builtins.head (pkgs.lib.splitString " " self.nixosConfigurations.example.config.systemd.services.autoland-review.serviceConfig.ExecStart)} check
  touch $out
''
