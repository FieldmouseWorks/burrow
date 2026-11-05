# A switch reaches the turns already waiting in line, each in its place.
{ self, pkgs, ... }:
import ../tests/proj-deployed.nix { inherit self pkgs; }
