# The MCP servers burrow's modules give Claude: every session of burrow.user
# gets them, in its user-scope config (~/.claude.json), through Claude's own
# CLI, the one way to change that config while sessions are reading it.
{ config, lib, pkgs, ... }:
let
  inherit (lib) mkOption types;
  inherit (config.burrow) user;
  servers = config.burrow.claude.mcpServers;
  # Each as Claude keeps it, to tell one that's already there.
  json = builtins.toJSON (lib.mapAttrs (_: s: { type = "stdio"; inherit (s) command args; }) servers);
  register = pkgs.writeShellApplication {
    name = "burrow-claude-mcp";
    runtimeInputs = [ config.burrow.claude.package pkgs.jq pkgs.systemd ];
    text = ''
      servers=${lib.escapeShellArg json}
      mapfile -t names < <(jq -r 'keys[]' <<<"$servers")
      case ''${1:?add or remove} in
        add)
          for n in "''${names[@]}"; do
            want=$(jq -c --arg n "$n" '.[$n]' <<<"$servers")
            jq -e --arg n "$n" --argjson w "$want" '.mcpServers[$n] == $w' ~/.claude.json >/dev/null && continue
            claude mcp remove -s user "$n" >/dev/null 2>&1 || true
            claude mcp add-json -s user "$n" "$want"
          done ;;
        remove)
          # Shutting down isn't turning them off.
          [ "$(systemctl is-system-running || true)" = stopping ] && exit 0
          for n in "''${names[@]}"; do
            # One already gone by hand is as good as removed.
            jq -e --arg n "$n" '.mcpServers | has($n)' ~/.claude.json >/dev/null || continue
            claude mcp remove -s user "$n"
          done ;;
      esac
    '';
  };
  home = config.users.users.${user}.home;
in
{
  options.burrow.claude.mcpServers = mkOption {
    type = types.attrsOf (types.submodule {
      options = {
        command = mkOption {
          type = types.str;
          example = "/run/current-system/sw/bin/burrow-browser-mcp";
          description = ''
            What Claude starts, over stdio: a path that stays put when its
            package changes, so Claude's config doesn't.
          '';
        };
        args = mkOption {
          type = types.listOf types.str;
          default = [ ];
        };
      };
    });
    default = { };
    description = ''
      MCP servers every Claude session of burrow.user gets, by name. Added
      once Claude has a config, and removed once they leave this option.
    '';
  };

  # Waits for Claude to have a config: making one would read as logged in. A
  # change stops the old unit, which removes the old servers, and starts the
  # new one, which adds the new.
  config = lib.mkIf (servers != { }) {
    systemd.services.burrow-claude-mcp = {
      description = "Give Claude Code burrow's MCP servers";
      wantedBy = [ "multi-user.target" ];
      unitConfig.ConditionPathExists = "${home}/.claude.json";
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        User = user;
        ExecStart = "${lib.getExe register} add";
        ExecStop = "${lib.getExe register} remove";
      };
    };
    systemd.paths.burrow-claude-mcp = {
      wantedBy = [ "paths.target" ];
      pathConfig.PathExists = "${home}/.claude.json";
    };
  };
}
