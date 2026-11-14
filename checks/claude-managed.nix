# Managed settings carry permissions, guards and safety, never a model or an
# effort: those are the live role settings (`proj settings show`), which proj
# passes to each turn, and a managed value would outrank them. The flake's
# `burrow.claude.effort` is only where the settings start.
{ self, pkgs, ... }:
let
  fixture = self.nixosConfigurations.example.extendModules {
    modules = [ { burrow.claude.effort = "high"; } ];
  };
  inherit (fixture) config;
  managed = builtins.fromJSON (builtins.unsafeDiscardStringContext config.environment.etc."claude-code/managed-settings.json".text);
  pins = builtins.filter (name: builtins.hasAttr name managed) [ "model" "effortLevel" "availableModels" "modelOverrides" ];
  envPins = builtins.filter (name: builtins.hasAttr name (managed.env or { })) [
    "ANTHROPIC_MODEL"
    "CLAUDE_CODE_EFFORT_LEVEL"
    "CLAUDE_CODE_SUBAGENT_MODEL"
    "CLAUDE_CODE_SUBAGENT_MODEL_FORCE"
  ];
in
assert pins == [ ] && envPins == [ ];
# Kept: the permissions and the guard.
assert managed.permissions.defaultMode == "bypassPermissions" && managed.permissions.ask != [ ];
assert managed ? hooks;
# The starting default is what the live settings read.
assert config.burrow.settings.effective.claudeEffort == "high";
pkgs.runCommand "claude-managed-check" { } "touch $out"
