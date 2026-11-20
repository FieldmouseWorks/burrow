# The flow's seal keys reach its controller alone, never the store (VM test).
{ self, pkgs, ... }:
import ../tests/autoland-flow-keys.nix { inherit self pkgs; } // {
  # The test it runs, for the negative run (lib/project.nix's checks).
  testFiles = [ "tests/autoland-flow-keys.nix" ];
}
