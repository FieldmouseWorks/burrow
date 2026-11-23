# modules/engines/antigravity.nix wires Antigravity into burrow.agents.engines
# cleanly alongside Claude without touching Claude's entry, and its packaged
# Conary v5 plugin passes Conary's runner check (`Plugin::check`).
{ self, pkgs, ... }:
let
  standIn = pkgs.writeShellScriptBin "antigravity" ''exit 0'';
  machine = self.nixosConfigurations.example.extendModules {
    modules = [ {
      burrow.engines.antigravity = {
        enable = true;
        package = standIn;
        credentialFile = "/run/secrets/antigravity-agent-credential";
        hosts = [ "antigravity.example" ];
        models = [
          {
            id = "test-model";
            efforts = [ "low" "medium" "high" "xhigh" "max" ];
          }
        ];
        defaultModel = "test-model";
        defaultEffort = "high";
        meters = [ "quota" ];
      };
    } ];
  };
  agent = machine.config.burrow.agents.package;
  plugin = machine.config.burrow.engines.antigravity.pluginPackage;
  call = "${self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-example-plugin}/bin/burrow-example-plugin-call";
in
pkgs.runCommand "engine-antigravity-config-check" { nativeBuildInputs = [ pkgs.jq ]; } ''
  config=${agent.config}
  holds() { jq -e "$1" "$config" >/dev/null || { echo "agent config: not $1"; exit 1; }; }
  holds '.engines.antigravity != null'
  holds '.engines.claude != null'
  holds '.engines.antigravity.credentialFile == "/run/secrets/antigravity-agent-credential"'
  holds '.engines.antigravity.plugin != null'
  holds '.engines.antigravity.mcpServers == []'
  holds '.engines.claude.credentialFile == null'

  pluginCfg=${plugin.config}
  jq -e '.models == [{id: "test-model", efforts: ["low", "medium", "high", "xhigh", "max"], context_tokens: null, max_output_tokens: null}]' "$pluginCfg" >/dev/null
  jq -e '.default_model == "test-model" and .default_effort == "high" and .network == true and .mcp == false and .devices == [] and .meters == ["quota"]' "$pluginCfg" >/dev/null

  checked=$(${call} check ${pkgs.lib.getExe plugin})
  [ "$checked" = '["job"]' ] || { echo "unexpected checked kinds: $checked"; exit 1; }
  touch $out
''
