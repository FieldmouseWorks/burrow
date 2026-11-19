# The proof producer's units, users and polkit rule, for real (VM test).
{ self, pkgs, ... }:
import ../tests/proof-producer.nix { inherit self pkgs; }
