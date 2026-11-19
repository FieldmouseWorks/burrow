# The flow's controller user: only it can reach a producer (VM test).
{ self, pkgs, ... }:
import ../tests/ledger-controller.nix { inherit self pkgs; }
