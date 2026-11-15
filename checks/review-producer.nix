# The review producer's units, users and handoff with the reviewers'
# service, for real (VM test).
{ self, pkgs, ... }:
import ../tests/review-producer.nix { inherit self pkgs; }
