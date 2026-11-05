# The ledger's pass over one repo, with a stand-in for cn.
{ self, pkgs, ... }:
import ../tests/ledger.nix { inherit self pkgs; }
