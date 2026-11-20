# The judge producer's looks, units, users and polkit rule, for real, and
# burrow.user refused at every way to answer in the judge's place (VM test).
{ self, pkgs, ... }:
import ../tests/judge-look.nix { inherit self pkgs; }
