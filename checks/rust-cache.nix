# The shared compile cache: one daemon per user, the compiles in the
# builds that ask for them, and a plain compile whenever it can't help.
{ self, pkgs, inputs, ... }:
import ../tests/rust-cache.nix { inherit self pkgs inputs; }
