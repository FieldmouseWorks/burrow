# The steward and its reviewers, with stand-ins for gh, git's remote and
# the agents, and its one-way paths as the module sets them; a look it
# queues is taken by the reviewers' own service, burrow-review, with
# its tests' stand-ins for codex and bubblewrap (they need bash and jq).
# Its passes run burrow-upkeep, whose source says what it lists, and
# start a proof run through burrow-proof, which reads what they write.
{ self, pkgs, system, ... }:
pkgs.runCommand "autoland-check" { nativeBuildInputs = [ pkgs.python3 pkgs.git pkgs.openssl pkgs.bash pkgs.jq ]; } ''
  python3 -I ${../tests/autoland}/check.py ${../modules/autoland}/steward.py \
    ${pkgs.writeText "one-way.json" (builtins.toJSON self.nixosConfigurations.example.config.burrow.autoland.oneWay)} \
    ${self.packages.${system}.burrow-review}/bin/burrow-review ${../crates/burrow-review/tests/fakes} \
    ${self.packages.${system}.burrow-upkeep}/bin/burrow-upkeep ${../crates/burrow-upkeep/src} \
    ${self.packages.${system}.burrow-proof}/bin/burrow-proof
  touch $out
''
