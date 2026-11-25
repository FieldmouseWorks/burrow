# renovate-sweep's seal key reaches conaryd's flow, its controller's alone,
# never the store, and a new one again (VM test).
{ self, pkgs, ... }:
import ../tests/autoland-major-key.nix { inherit self pkgs; } // {
  # The test it runs, for the negative run (lib/project.nix's checks).
  testFiles = [ "tests/autoland-major-key.nix" ];
}
