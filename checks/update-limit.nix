# HOST-update's waits for a heavy slot can be handed back to its time
# limit: systemd takes that only of a unit with NotifyAccess, and the run
# hands back against the limit the unit has.
{ self, pkgs, ... }:
let
  machine = self.nixosConfigurations.example.config;
  tool = machine.burrow.tools.update;
  unit = machine.systemd.services.${tool.meta.mainProgram or tool.name}.serviceConfig;
  limit = toString unit.TimeoutStartSec;
in
assert unit.Type == "oneshot" && (unit.NotifyAccess or "none") == "main";
pkgs.runCommand "update-limit-check" { nativeBuildInputs = [ pkgs.jq ]; } ''
  config=$(grep -aom1 '/nix/store/[a-z0-9]\{32\}-update\.json' ${unit.ExecStart} | head -n1)
  [ "$(jq -r .workLimit "$config")" = "${limit}" ] || { echo "update.json's workLimit isn't the unit's ${limit}"; exit 1; }
  touch $out
''
