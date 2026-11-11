# Renovate failures are handed to jobs, which need both journal scopes.
{ self, pkgs, nixpkgs, ... }:
let
  lib = nixpkgs.lib;
  machine = self.nixosConfigurations.example;
  globs = machine.config.burrow.probe.journal.units;
  regex = lib.replaceStrings [ "." "*" "?" ] [ "\\." ".*" "." ];
  on = unit: lib.any (g: builtins.match (regex g) unit != null) globs;
  units = [ "renovate.service" "renovate@sam-homelab.service" ];
  wrong = lib.filter (u: !on u) units;
  unrelated = [ "sshd.service" "renovate-secrets.service" "unrelated.service" ];
in
assert lib.assertMsg (machine.config.systemd.services ? renovate
  && machine.config.systemd.services ? "renovate@") "probe-renovate: example must enable both Renovate units";
assert lib.assertMsg (lib.all (u: !on u) unrelated) "probe-renovate: unrelated journals must remain denied";
pkgs.runCommand "probe-renovate-check" { } ''
  ${lib.concatMapStrings (u: "echo '${u}: not on the probe allowlist'\n") wrong}
  [ ${toString (builtins.length wrong)} = 0 ]
  touch $out
''
