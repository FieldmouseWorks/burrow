# Every name an owner's list (burrow.agents.engines.NAME.capabilities) can
# hold, as crates/burrow-grants names them, for the VM tests that approve
# them (../agents-plugins.nix, ../../lib/engine-conformance.nix).
{
  # What a plugin can be granted.
  grantable = [
    "workspace" "job-state" "git-metadata" "toolchains" "system" "probe"
    "units-clear" "heavy" "nix" "kvm" "mcp" "network"
  ];
  # What no owner's list can grant.
  withheld = [ "github" "environment" "github-token" "github-installation" "settings-write" "plugin-install" ];
}
