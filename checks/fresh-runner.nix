# A runner started fresh for one job: its registration and state
# gone however it ends, and a clean end not failed.
{ self, pkgs, ... }:
import ../tests/fresh-runner.nix { inherit self pkgs; }
