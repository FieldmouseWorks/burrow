# Wrap burrow-engine-anthropic with a JSON runtime config file for one
# Anthropic mode ("claude" or "claude-api"). The wrapper takes no arguments,
# so Conary's runner and burrow.agents.plugins invoke it directly.
{
  self,
  pkgs,
  lib ? pkgs.lib,
  mode,
  program ? null,
  credentialEnv ? null,
  models ? [ ],
  defaultModel ? null,
  defaultEffort ? null,
  capabilities ? null,
  meters ? null,
  apiBaseUrl ? null,
  maxBudgetUsd ? null,
  credit ? null,
  appendSystemPrompt ? null,
}:
let
  bin = self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-engine-anthropic;
  renderModel =
    m:
    {
      inherit (m) id;
      efforts = m.efforts or [ ];
    }
    // lib.optionalAttrs ((m.contextTokens or null) != null) {
      context_tokens = m.contextTokens;
    }
    // lib.optionalAttrs ((m.maxOutputTokens or null) != null) {
      max_output_tokens = m.maxOutputTokens;
    };
  pluginConfig = pkgs.writeText "burrow-engine-anthropic-${mode}.json" (
    builtins.toJSON (
      {
        inherit mode;
        models = map renderModel models;
      }
      // lib.optionalAttrs (defaultModel != null) { inherit defaultModel; }
      // lib.optionalAttrs (defaultEffort != null) { inherit defaultEffort; }
      // lib.optionalAttrs (capabilities != null) { inherit capabilities; }
      // lib.optionalAttrs (meters != null) { inherit meters; }
      // lib.optionalAttrs (program != null) { inherit program; }
      // lib.optionalAttrs (credentialEnv != null) { inherit credentialEnv; }
      // lib.optionalAttrs (apiBaseUrl != null) { inherit apiBaseUrl; }
      // lib.optionalAttrs (maxBudgetUsd != null) { inherit maxBudgetUsd; }
      // lib.optionalAttrs (credit != null) { inherit credit; }
      // lib.optionalAttrs (appendSystemPrompt != null) { inherit appendSystemPrompt; }
    )
  );
in
pkgs.runCommand "burrow-engine-anthropic-${mode}"
  {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = "burrow-engine-anthropic-${mode}";
    passthru.config = pluginConfig;
  }
  ''
    makeBinaryWrapper ${lib.getExe bin} $out/bin/burrow-engine-anthropic-${mode} \
      --add-flags "--config ${pluginConfig}"
  ''
