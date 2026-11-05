# burrow-events for real: deliveries, kicks and wake-ups starting a
# handler's units through polkit.
{ self, pkgs, ... }:
import ../tests/events.nix { inherit self pkgs; }
