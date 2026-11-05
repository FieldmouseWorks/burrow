# A probe can read the steward's journals: every unit autoland.nix
# defines (a template's instances, for one) is on the example's
# allowlist, so a renamed or new steward unit can't fall off it.
{ self, pkgs, nixpkgs, ... }:
let
  lib = nixpkgs.lib;
  machine = self.nixosConfigurations.example;
  globs = machine.config.burrow.probe.journal.units;
  # As burrow-events' glob reads them: `*` any run, `?` any one, a
  # `.` itself (no unit name or allowlist entry holds another
  # character a regex reads otherwise).
  regex = lib.replaceStrings [ "." "*" "?" ] [ "\\." ".*" "." ];
  on = unit: lib.any (g: builtins.match (regex g) unit != null) globs;
  defined = lib.filter (d: lib.hasSuffix "/modules/autoland.nix" (toString d.file))
    machine.options.systemd.services.definitionsWithLocations;
  units = map (u: if lib.hasSuffix "@" u then "${u}sam-homelab.service" else "${u}.service")
    (lib.concatMap (d: lib.attrNames d.value) defined);
  wrong = lib.filter (u: !on u) units;
in assert lib.assertMsg (builtins.length units >= 3) "probe-steward: autoland.nix's units: ${builtins.toJSON units}";
pkgs.runCommand "probe-steward-check" { } ''
  ${lib.concatMapStrings (u: "echo '${u}: not on the probe allowlist'
") wrong}
  [ ${toString (builtins.length wrong)} = 0 ]
  touch $out
''
