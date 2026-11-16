# The example's reviewers' service reads the config its module writes
# (a field the binary doesn't know fails it).
{ self, pkgs, ... }:
let
  machine = self.nixosConfigurations.example.config;
  lib = pkgs.lib;
  # Both membership spellings, including the primary group, are refused.
  refused = group: membership: let
    bad = (self.nixosConfigurations.example.extendModules { modules = [
      ({ config, ... }: membership config.burrow.user group)
    ]; }).config;
  in lib.any (a: !a.assertion && lib.hasInfix "burrow.user must be neither" a.message) bad.assertions;
  memberships = [
    (user: group: { users.users.${user}.extraGroups = [ group ]; })
    (user: group: { users.users.${user}.group = lib.mkForce group; })
    (user: group: { users.groups.${group}.members = [ user ]; })
  ];
  unit = machine.systemd.services.autoland-review;
  deployedDir = pkgs.lib.removePrefix "/etc/" unit.environment.REVIEW_DEPLOYED;
  deployed = machine.environment.etc.${deployedDir}.source;
  program = builtins.head (pkgs.lib.splitString " " unit.serviceConfig.ExecStart);
in
# A retained reviewer must have a live policy source and the program that
# takes its queue after a switch: `check` holds the deployed generation to
# both (#711), and the service tests exercise the handover.
assert !unit.restartIfChanged;
assert lib.all (group: lib.all (refused group) memberships) [ "autoland-review" "autoland-review-askers" ];
pkgs.runCommand "review-config-check" { } ''
  REVIEW_DEPLOYED=${deployed} ${program} check
  touch $out
''
