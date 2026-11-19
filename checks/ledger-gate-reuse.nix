# The ledger's gate-reuse lookup, running on and off, with a stand-in for
# conaryd.
{ self, pkgs, ... }:
import ../tests/ledger-gate-reuse.nix { inherit self pkgs; }
