# Sandboxes for real: boot two, work in them, break out of nothing; and run
# an engine's plugin in one of its own, with its grants and nothing else.
# Nested KVM: run it on a machine that has /dev/kvm. One check, since a
# machine proves it by this name (CI only evaluates it).
{ self, pkgs, ... }:
pkgs.linkFarm "agents" {
  sandboxes = import ../tests/agents.nix { inherit self pkgs; };
  plugins = import ../tests/agents-plugins.nix { inherit self pkgs; };
}
