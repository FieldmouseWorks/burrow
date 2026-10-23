# robot: Hetzner's out-of-band controls (rescue system, hard reset, wake-on-LAN)
# for a Hetzner dedicated server, from the server itself or from anywhere with
# the Robot credentials (robot/robot.sh). Cancelling and reinstalling are left
# out on purpose, and the guard (claude/guard.sh) asks before a raw call to them.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.robot;

  robot = pkgs.writeShellApplication {
    name = "robot";
    runtimeInputs = [ pkgs.curl pkgs.jq pkgs.coreutils pkgs.gnused ];
    text = ''
      ROBOT_DEFAULT_IP=${lib.escapeShellArg cfg.serverIp}
      ROBOT_DEFAULT_CREDS=${lib.escapeShellArg cfg.credentialsFile}
    '' + builtins.readFile ./robot/robot.sh;
  };
in
{
  imports = [ (import ./common.nix inputs) ];

  options.burrow.robot = {
    enable = lib.mkEnableOption "robot, the Hetzner Robot CLI";

    serverIp = lib.mkOption {
      type = lib.types.str;
      example = "203.0.113.7";
      description = "The server's main IPv4 address, which is how Robot names it.";
    };

    credentialsFile = lib.mkOption {
      type = lib.types.str;
      default = "/run/secrets/robot";
      description = ''
        A file holding "user:password" for a Robot webservice user (Robot >
        Settings > Webservice and app settings). Keep it readable by wheel only;
        robot falls back to sudo to read it.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ robot ];

    burrow.claude.machineMd = lib.mkOrder 1300 ''
      ## When ${config.networking.hostName} is in trouble

      `robot status | rescue on | rescue off | reset [sw|hw|power] | wol` drives
      Hetzner's out-of-band controls. It works from any machine with the Robot
      credentials, which is the way back in when ${config.networking.hostName} itself is down.
    '';
  };
}
