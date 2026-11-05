# The signed-in browser: Claude driving it, its logins kept, its ports
# and profile kept from everyone else.
{ self, pkgs, ... }:
import ../tests/browser.nix { inherit self pkgs; }
