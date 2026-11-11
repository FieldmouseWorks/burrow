# The scheduler's own account can read and resume deferred engine requests.
{ self, pkgs, ... }:
import ../tests/engines-door.nix { inherit self pkgs; }
