# Claude Code moves fast, so it tracks nixpkgs-unstable on its own. One
# definition for the host and the agent sandboxes, so both run the same build.
{ inputs, system }:
(import inputs.nixpkgs-unstable {
  inherit system;
  config.allowUnfree = true;
}).claude-code
