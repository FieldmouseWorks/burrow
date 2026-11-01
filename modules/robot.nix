# robot: Hetzner's out-of-band controls (rescue system, hard reset, wake-on-LAN)
# for a Hetzner dedicated server, from the server itself or from anywhere with
# the Robot credentials (crates/burrow-robot). Cancelling and reinstalling are left
# out on purpose, and the guard (crates/burrow-guard) asks before a raw call to them.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.robot;

  # What crates/burrow-robot reads (its Config), and the command with it
  # built in. sudo, for a credentials file only root may read, is the
  # caller's own.
  robotConfig = pkgs.writeText "robot.json" (builtins.toJSON {
    inherit (cfg) serverIp credentialsFile;
  });
  robot = pkgs.runCommand "robot" {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = "robot";
  } ''
    makeBinaryWrapper ${inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow}/bin/burrow-robot \
      $out/bin/robot --add-flags ${robotConfig}
  '';
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
    burrow.tools.robot = robot;

    burrow.claude.machineMd = lib.mkOrder 1300 ''
      ## When ${config.networking.hostName} is in trouble

      `robot status | rescue on | rescue off | reset [sw|hw|power] | wol` drives
      Hetzner's out-of-band controls. It works from any machine with the Robot
      credentials, which is the way back in when ${config.networking.hostName} itself is down.
    '';
  };
}
