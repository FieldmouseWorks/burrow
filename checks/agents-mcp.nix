# The agent's config, as the module writes it, from a machine with the
# signed-in browser and another MCP server: the browser says it acts as the
# owner, an engine's approved servers reach burrow-agent, and nothing of a
# host job's (envVars, host paths) comes with them.
{ self, pkgs, ... }:
let
  machine = self.nixosConfigurations.example.extendModules {
    modules = [ {
      burrow.mcpServers.notes = {
        command = "/run/current-system/sw/bin/notes-mcp";
        env.SETTING = "value";
        envVars = [ "GH_TOKEN" ];
        readPaths = [ "/run/notes" ];
      };
      burrow.agents.engines.claude.mcpServers = [ "notes" ];
    } ];
  };
  agent = machine.config.burrow.agents.package;
in
pkgs.runCommand "agents-mcp-check" { nativeBuildInputs = [ pkgs.jq ]; } ''
  config=${agent.config}
  holds() { jq -e "$1" "$config" >/dev/null || { echo "agent config: not $1"; exit 1; }; }
  holds '.mcpServers.browser.actsAsOwner'
  holds '.mcpServers.notes == {command: "/run/current-system/sw/bin/notes-mcp", args: [], env: {SETTING: "value"}, actsAsOwner: false}'
  holds '[.mcpServers[] | keys == ["actsAsOwner", "args", "command", "env"]] | all'
  holds '.engines.claude.mcpServers == ["notes"]'
  # It refuses a field it doesn't know, and one that's missing.
  ${pkgs.lib.getExe agent} ls
  touch $out
''
