# The steward, with stand-ins for gh, git's remote and Claude jobs, and
# the class policy the module gives Conary; Codex's connector's posts are
# in the PRs it reads. Its passes run burrow-upkeep, whose source says what it lists, and
# start a proof run through burrow-proof, which reads what they write; its
# tasks read their policy text from burrow-brief, and its briefs are held
# to that crate's goldens. Each
# stage the steward names has a label in the module (AUTOLAND_STAGES).
{ self, pkgs, system, ... }:
pkgs.runCommand "autoland-check" {
  nativeBuildInputs = [ pkgs.python3 pkgs.git pkgs.openssl pkgs.bash pkgs.jq ];
  # The stages labels.nix has a label for, which must be the steward's own.
  AUTOLAND_STAGES = builtins.toJSON (builtins.attrNames self.nixosConfigurations.example.config.burrow.autoland.labels.waiting);
} ''
  python3 ${../tests/autoland}/lifecycle.py ${../modules/autoland} ${self.packages.${system}.burrow-brief}/bin/burrow-brief
  python3 -I ${../tests/autoland}/check.py ${../modules/autoland}/steward.py \
    ${self.nixosConfigurations.example.config.burrow.ledger.env.CONARY_POLICY} \
    ${self.packages.${system}.burrow-upkeep}/bin/burrow-upkeep ${../crates/burrow-upkeep/src} \
    ${self.packages.${system}.burrow-proof}/bin/burrow-proof ${../crates/burrow-proj/src/ask.rs} \
    ${self.packages.${system}.burrow-settings}/bin/burrow-settings \
    ${self.packages.${system}.burrow-brief}/bin/burrow-brief ${../crates/burrow-brief/tests/golden}
  touch $out
''
