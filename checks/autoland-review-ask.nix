# The owner's door's broker: the flow's controller and the steward ask
# through it and get the door's answer, the door runs as burrow.user with
# none of the asker's environment, and anyone else is refused (VM test).
{ self, pkgs, ... }:
import ../tests/autoland-review-ask.nix { inherit self pkgs; } // {
  # The test it runs, for the negative run (lib/project.nix's checks).
  testFiles = [ "tests/autoland-review-ask.nix" ];
}
