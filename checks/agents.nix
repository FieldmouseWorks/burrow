# Sandboxes for real: boot two, work in them, break out of nothing.
# Nested KVM: run it on a machine that has /dev/kvm.
{ self, pkgs, ... }:
import ../tests/agents.nix { inherit self pkgs; }
