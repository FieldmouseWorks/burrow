# Probes answer on a private repo's issues, never read a unit off the
# allowlist, and post a planted token scrubbed.
{ self, pkgs, ... }:
import ../tests/probe.nix { inherit self pkgs; }
