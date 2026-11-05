# proj's turns and memory caps, with a stand-in for Claude; a turn's
# second run, once its first is stopped; who waits in line for its
# slots; and its GitHub side, with a stand-in gh. Four tests on one
# machine's config, so they run side by side.
{ self, pkgs, ... }:
import ../tests/proj.nix { inherit self pkgs; }
