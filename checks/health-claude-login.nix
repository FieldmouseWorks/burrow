# HOST-health's claude-login check: an expired Claude login fails it by name.
{ self, pkgs, ... }:
import ../tests/health-claude-login.nix { inherit self pkgs; }
