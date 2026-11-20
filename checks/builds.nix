# The build store: uploads through nginx's door, substituted from it.
{ self, pkgs, ... }:
import ../tests/builds.nix { inherit self pkgs; }
