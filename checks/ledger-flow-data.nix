# The flow's state carried over as its controller is turned on, changed
# and turned off (VM test).
{ self, pkgs, ... }:
import ../tests/ledger-flow-data.nix { inherit self pkgs; } // {
  # The tests it runs, for the negative run (lib/project.nix's checks).
  testFiles = [ "tests/ledger-flow-data.nix" ];
}
