# The Anthropic Nix engine surface (modules/engines/anthropic.nix): both
# `claude` and `claude-api` are wired through burrow.agents.engines with their
# credentials, Anthropic egress hosts, distinct proxy ports, and packaged
# burrow-engine-anthropic plugin wrappers whose `describe` reflects the
# runtime models and efforts configured in Nix (never hardcoded model IDs).
{ self, pkgs, ... }:
let
  machine = self.nixosConfigurations.example.extendModules {
    modules = [
      {
        burrow.claude = {
          effort = "high";
          plugin = {
            models = [
              {
                id = "runtime-model-a";
                efforts = [
                  "low"
                  "medium"
                  "high"
                ];
                contextTokens = 200000;
                maxOutputTokens = 16384;
              }
              {
                id = "runtime-model-b";
                efforts = [ "low" ];
              }
            ];
            defaultModel = "runtime-model-a";
          };
          api = {
            keyFile = "/run/secrets/anthropic-api-key";
            maxBudgetUsd = 5000000;
          };
        };
      }
    ];
  };
  engines = machine.config.burrow.agents.engines;
  claudePlugin = pkgs.lib.getExe engines.claude.plugin;
  apiPlugin = pkgs.lib.getExe engines.claude-api.plugin;
  agentConfig = machine.config.burrow.agents.package.config;
  describeReq = builtins.toJSON {
    contract = "conary-plugin v5";
    kind = "describe";
  };
in
pkgs.runCommand "anthropic-plugin-check" { nativeBuildInputs = [ pkgs.jq ]; } ''
  # Both Anthropic modes appear in burrow-agent's rendered config with their
  # plugins and credentials wired cleanly.
  jq -e '.engines.claude.plugin != null and .engines["claude-api"].plugin != null' ${agentConfig} >/dev/null
  jq -e '.engines["claude-api"].credentialFile == "/run/secrets/anthropic-api-key"' ${agentConfig} >/dev/null

  # Distinct proxy ports and identical Anthropic egress hosts.
  test "${toString engines.claude.proxyPort}" = "3128"
  test "${toString engines.claude-api.proxyPort}" = "3136"
  test "${engines.claude.credentialEnv}" = "CLAUDE_CODE_OAUTH_TOKEN"
  test "${engines.claude-api.credentialEnv}" = "ANTHROPIC_API_KEY"

  # Both packaged plugins answer `describe` offline with the Nix-configured
  # models, efforts, capabilities (sandbox: needs, network + mcp, no kvm),
  # and mode-specific meters.
  claude_desc=$(${claudePlugin} <<<${pkgs.lib.escapeShellArg describeReq})
  jq -e '
    .by.engine == "claude"
    and .v2_kinds == ["job"]
    and (.models | map(.id)) == ["runtime-model-a", "runtime-model-b"]
    and .models[0].efforts == ["low", "medium", "high"]
    and .models[0].context_tokens == 200000
    and .models[0].max_output_tokens == 16384
    and .capabilities == {sandbox: "needs", network: true, devices: [], mcp: true}
    and .meters == ["five_hour", "seven_day"]
  ' <<<"$claude_desc" >/dev/null

  api_desc=$(${apiPlugin} <<<${pkgs.lib.escapeShellArg describeReq})
  jq -e '
    .by.engine == "claude-api"
    and .v2_kinds == ["job"]
    and (.models | map(.id)) == ["runtime-model-a", "runtime-model-b"]
    and .capabilities == {sandbox: "needs", network: true, devices: [], mcp: true}
    and .meters == ["credit"]
  ' <<<"$api_desc" >/dev/null

  # Verify the generated JSON configs carry the inherited defaultEffort and
  # API budget without hardcoding anything in the binary.
  jq -e '.defaultModel == "runtime-model-a" and .defaultEffort == "high"' ${engines.claude.plugin.config} >/dev/null
  jq -e '.defaultModel == "runtime-model-a" and .defaultEffort == "high" and .maxBudgetUsd == 5000000' ${engines.claude-api.plugin.config} >/dev/null

  touch $out
''
