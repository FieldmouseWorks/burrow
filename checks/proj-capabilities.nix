# One job contract, exercised through both engine command builders.
{ self, pkgs, ... }:
import ../tests/proj-capabilities.nix { inherit self pkgs; }
