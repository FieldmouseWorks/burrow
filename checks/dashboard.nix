# The dashboard: on localhost alone, sign-in first, enrolment by code.
{ self, pkgs, ... }:
import ../tests/dashboard.nix { inherit self pkgs; }
