# The example's house-sync reads the config its module writes (a
# field the binary doesn't know fails it). The sandbox has no
# /etc/burrow/house, so a pass is one for other blocks: it says so and
# does nothing, before any gh call or lock.
{ self, pkgs, system, ... }:
let
  sync = pkgs.lib.getExe (builtins.head (builtins.filter (p: (p.name or "") == "house-sync")
    self.nixosConfigurations.example.config.environment.systemPackages));
in pkgs.runCommand "house-sync-config-check" { } ''
  got=$(${sync} status)
  [ "$got" = "house-sync: the system has other house blocks than this pass; the pass for those does the work" ] ||
    { echo "got: $got"; exit 1; }
  touch $out
''
