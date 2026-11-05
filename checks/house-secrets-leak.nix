# lib/project.nix's secrets check fails a project with a token in any
# tracked text file, and its log says where, never what.
{ self, pkgs, ... }:
import ../tests/house-secrets.nix { inherit self pkgs; }
