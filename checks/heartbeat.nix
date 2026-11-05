# heartbeat's beat as the example builds it, with a stand-in HOST-health.
{ self, pkgs, ... }:
import ../tests/heartbeat.nix { inherit self pkgs; }
