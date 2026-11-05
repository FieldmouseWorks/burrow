# A proof run in the unit the steward starts: each command fenced,
# the whole run bounded, and all of it ended however it ends.
{ self, pkgs, ... }:
import ../tests/proof-run.nix { inherit self pkgs; }
