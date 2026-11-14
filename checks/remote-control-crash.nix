# A crashing Remote Control server restarts as RestartSec paces it, not as claude-rc-scan does.
{ self, pkgs, ... }:
import ../tests/remote-control-crash.nix { inherit self pkgs; }
