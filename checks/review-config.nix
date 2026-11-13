# The example's reviewers' service reads the config its module writes
# (a field the binary doesn't know fails it).
{ self, pkgs, ... }:
let
  machine = self.nixosConfigurations.example.config;
  unit = machine.systemd.services.autoland-review;
  deployedDir = pkgs.lib.removePrefix "/etc/" unit.environment.REVIEW_DEPLOYED;
  deployed = machine.environment.etc.${deployedDir}.source;
  program = builtins.head (pkgs.lib.splitString " " unit.serviceConfig.ExecStart);
in
# A retained reviewer must have a live policy source and the program that
# takes its queue after a switch: `check` holds the deployed generation to
# both (#711), and the service tests exercise the handover.
assert !unit.restartIfChanged;
pkgs.runCommand "review-config-check" { } ''
  REVIEW_DEPLOYED=${deployed} ${program} check
  touch $out
''
