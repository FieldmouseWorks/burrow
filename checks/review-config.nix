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
# takes its queue after a switch. The service tests exercise that handover;
# this check catches a module that stops supplying the deployed generation.
assert !unit.restartIfChanged;
pkgs.runCommand "review-config-check" { } ''
  ${program} check
  [ "$(readlink -f ${deployed}/burrow-review)" = "${program}" ]
  exec() { :; }
  unset REVIEW_CONFIG
  source ${program}
  [ "$REVIEW_CONFIG" = "$(readlink -f ${deployed}/config.json)" ]
  ${pkgs.jq}/bin/jq -e '.settings != null' "$REVIEW_CONFIG"
  touch $out
''
