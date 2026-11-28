# modules/engines/antigravity.nix wires Antigravity into burrow.agents.engines
# cleanly alongside Claude without touching Claude's entry, and its packaged
# Conary v5 plugin passes Conary's runner check (`Plugin::check`).
{ self, pkgs, ... }:
let
  standIn = pkgs.writeShellScriptBin "antigravity" "exit 0";
  machine = self.nixosConfigurations.example.extendModules {
    modules = [
      {
        burrow.engines.antigravity = {
          enable = true;
          package = standIn;
          credentialFile = "/run/secrets/antigravity-agent-credential";
          hosts = [ "antigravity.example" ];
          models = [
            {
              id = "test-model";
              efforts = [
                "low"
                "medium"
                "high"
                "xhigh"
                "max"
              ];
            }
          ];
          defaultModel = "test-model";
          defaultEffort = "high";
          meters = [ "quota" ];
        };
      }
    ];
  };
  # `mcp` in capabilities is refused: the plugin hands the CLI no servers.
  withMcp = machine.extendModules {
    modules = [
      {
        burrow.engines.antigravity.capabilities = [
          "workspace"
          "mcp"
        ];
      }
    ];
  };
  mcpRefused = pkgs.lib.any (
    a:
    !a.assertion
    && pkgs.lib.hasPrefix "burrow.engines.antigravity: `mcp` can't be in capabilities" a.message
  ) withMcp.config.assertions;
  withHomeFile = machine.extendModules {
    modules = [
      {
        burrow.engines.antigravity = {
          credentialMode = "home-file";
          credentialHomePath = ".config/antigravity/oauth-token";
        };
      }
    ];
  };
  homeFileValid = builtins.all (a: a.assertion) withHomeFile.config.assertions;
  withDedicatedHost = withHomeFile.extendModules {
    modules = [
      {
        burrow.engines.antigravity.host = {
          enable = true;
          credentialFile = "/run/secrets/antigravity-host-credential";
        };
      }
    ];
  };
  dedicatedHostValid = builtins.all (a: a.assertion) withDedicatedHost.config.assertions;
  hostMissingCredRefused =
    pkgs.lib.any
      (a: !a.assertion && pkgs.lib.hasPrefix "burrow.engines.antigravity.host.credentialFile" a.message)
      (withHomeFile.extendModules { modules = [ { burrow.engines.antigravity.host.enable = true; } ]; })
      .config.assertions;
  hostSharedCredRefused =
    pkgs.lib.any
      (a: !a.assertion && pkgs.lib.hasPrefix "burrow.engines.antigravity.host.credentialFile" a.message)
      (withHomeFile.extendModules {
        modules = [
          {
            burrow.engines.antigravity.host = {
              enable = true;
              credentialFile = "/run/secrets/antigravity-agent-credential";
            };
          }
        ];
      }).config.assertions;
  badHomePaths = [
    ""
    "/etc/passwd"
    "../escape"
    "."
    ".gemini/./token"
    ".gemini/../token"
    ".gemini/token/"
  ];
  badHomePathsRefused = builtins.all (
    bad:
    let
      badMachine = machine.extendModules {
        modules = [
          {
            burrow.engines.antigravity = {
              credentialMode = "home-file";
              credentialHomePath = bad;
            };
          }
        ];
      };
    in
    pkgs.lib.any (
      a: !a.assertion && pkgs.lib.hasPrefix "burrow.engines.antigravity.credentialHomePath" a.message
    ) badMachine.config.assertions
  ) badHomePaths;
  agent = machine.config.burrow.agents.package;
  plugin = machine.config.burrow.engines.antigravity.pluginPackage;
  homeFilePlugin = withHomeFile.config.burrow.engines.antigravity.pluginPackage;
  hostPlugin = withDedicatedHost.config.burrow.engines.antigravity.hostPluginPackage;
  call = "${
    self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-example-plugin
  }/bin/burrow-example-plugin-call";
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
  jq -e '.default_model == "test-model" and .default_effort == "high" and .network == true and (has("mcp") | not) and .devices == [] and .meters == ["quota"] and .deadline_secs == null' "$pluginCfg" >/dev/null
  jq -e '.credential_mode == "env" and .credential_home_path == ".gemini/antigravity-cli/antigravity-oauth-token"' "$pluginCfg" >/dev/null

  homeFileCfg=${homeFilePlugin.config}
  jq -e '.credential_mode == "home-file" and .credential_home_path == ".config/antigravity/oauth-token"' "$homeFileCfg" >/dev/null

  hostCfg=${hostPlugin.config}
  jq -e '.host_mode == true and .dangerously_skip_permissions == false and .mode == "accept-edits" and .credential_file == "/run/secrets/antigravity-host-credential" and (.settings_file | type == "string")' "$hostCfg" >/dev/null
  hostSettings=$(jq -r '.settings_file' "$hostCfg")
  jq -e '
    .toolPermission == "always-proceed"
    and (.permissions.deny | index("command(zfs destroy)") != null)
    and (.permissions.deny | index("command(sudo zfs destroy)") != null)
    and (.permissions.deny | index("command(regex:mkfs.*)") != null)
    and (.permissions.deny | index("command(regex:sudo mkfs.*)") != null)
    and (.permissions.deny | index("command(regex:fwupdmgr update.*)") != null)
    and (.permissions.deny | index("command(regex:git push --force origin main.*)") != null)
    and (.permissions.deny | index("command(mkfs)") == null)
  ' "$hostSettings" >/dev/null

  [ ${pkgs.lib.boolToString mcpRefused} = true ] || { echo "mcp in capabilities was not refused"; exit 1; }
  [ ${pkgs.lib.boolToString homeFileValid} = true ] || { echo "valid home-file config failed assertions"; exit 1; }
  [ ${pkgs.lib.boolToString dedicatedHostValid} = true ] || { echo "dedicated host credential config failed assertions"; exit 1; }
  [ ${pkgs.lib.boolToString hostMissingCredRefused} = true ] || { echo "host mode without host.credentialFile was not refused"; exit 1; }
  [ ${pkgs.lib.boolToString hostSharedCredRefused} = true ] || { echo "host mode sharing VM credentialFile was not refused"; exit 1; }
  [ ${pkgs.lib.boolToString badHomePathsRefused} = true ] || { echo "invalid credentialHomePath was not refused"; exit 1; }

  checked=$(${call} check ${pkgs.lib.getExe plugin})
  [ "$checked" = '["job"]' ] || { echo "unexpected checked kinds: $checked"; exit 1; }
  checkedHomeFile=$(${call} check ${pkgs.lib.getExe homeFilePlugin})
  [ "$checkedHomeFile" = '["job"]' ] || { echo "unexpected home-file checked kinds: $checkedHomeFile"; exit 1; }
  touch $out
''
