# Claude Code moves fast, so it tracks nixpkgs-unstable on its own. One
# definition for the host and the agent sandboxes, so both run the same build.
# With a manifest (burrow.claude.manifest), nixpkgs' recipe builds that release
# instead, so neither waits for nixpkgs to bump it.
{ inputs, system, manifest ? null }:
let
  claude-code = (import inputs.nixpkgs-unstable {
    inherit system;
    config.allowUnfree = true;
  }).claude-code;
in
if manifest == null then claude-code
else claude-code.override { manifest = builtins.fromJSON (builtins.readFile manifest); }
