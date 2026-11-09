# The steward and its reviewers, with stand-ins for gh, git's remote and
# the agents, and the class policy the module gives Conary; a look it
# queues is taken by the reviewers' own service, burrow-review, with
# its tests' stand-ins for codex and bubblewrap (they need bash and jq).
# Its passes run burrow-upkeep, whose source says what it lists, and
# start a proof run through burrow-proof, which reads what they write. Each
# stage the steward names has a label in the module (AUTOLAND_STAGES).
{ self, pkgs, system, ... }:
pkgs.runCommand "autoland-check" {
  nativeBuildInputs = [ pkgs.python3 pkgs.git pkgs.openssl pkgs.bash pkgs.jq ];
  # The stages labels.nix has a label for, which must be the steward's own.
  AUTOLAND_STAGES = builtins.toJSON (builtins.attrNames self.nixosConfigurations.example.config.burrow.autoland.labels.waiting);
} ''
  python3 -I ${../tests/autoland}/check.py ${../modules/autoland}/steward.py \
    ${self.nixosConfigurations.example.config.burrow.ledger.env.CONARY_POLICY} \
    ${self.packages.${system}.burrow-review}/bin/burrow-review ${../crates/burrow-review/tests/fakes} \
    ${self.packages.${system}.burrow-upkeep}/bin/burrow-upkeep ${../crates/burrow-upkeep/src} \
    ${self.packages.${system}.burrow-proof}/bin/burrow-proof ${../crates/burrow-proj/src/ask.rs}
  touch $out
''
