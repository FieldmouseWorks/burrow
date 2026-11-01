# HOST-notify: how the machine tells its owner something they must know, as
# one GitHub issue per problem (GitHub's app puts it on their phone), closed by
# itself when the problem clears (crates/burrow-notify). With no repo, notices
# go to the journal only. The trial boots and the update pipeline use it.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.notify;
  inherit (config.burrow) user;
  name = "${config.burrow.prefix}-notify";

  dir = "${config.burrow.stateDir}/notices";

  # What it's told, and the command with it built in. gh is the machine's
  # (as its own App, when it has one); id, runuser and chown run it as the
  # user, and systemd-cat writes the journal.
  notifyConfig = pkgs.writeText "${name}.json" (builtins.toJSON {
    inherit name dir user;
    inherit (cfg) repo assignee label;
    home = config.users.users.${user}.home;
  });
  notify = pkgs.runCommand name {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = name;
  } ''
    makeBinaryWrapper ${inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow}/bin/burrow-notify \
      $out/bin/${name} --add-flags ${notifyConfig} \
      --prefix PATH : ${lib.makeBinPath [ pkgs.coreutils pkgs.util-linux pkgs.systemd config.burrow.github.machineGh ]}
  '';
in
{
  imports = [
    (import ./common.nix inputs)
    { key = "burrow/github"; imports = [ (import ./github.nix inputs) ]; }
  ];

  options.burrow.notify = {
    enable = lib.mkEnableOption "HOST-notify, notices to the owner as GitHub issues";

    repo = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "sam/homelab";
      description = ''
        The GitHub repo (OWNER/NAME) the issues go to, opened as the machine's
        own App (burrow.github.machinery), else with burrow.user's gh login.
        null: notices only go to the journal.
      '';
    };

    assignee = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Who each issue is assigned to (a GitHub login), so it notifies them.";
    };

    label = lib.mkOption {
      type = lib.types.str;
      default = "updates";
      description = "The label every notice asks for; HOST-notify takes an issue with another label for someone else's.";
    };
  };

  config = lib.mkMerge [
    # Always built, so the modules that call it work whether or not it's on PATH.
    { burrow.tools.notify = notify; }

    (lib.mkIf cfg.enable {
      environment.systemPackages = [ notify ];
      systemd.tmpfiles.settings.burrow = {
        ${config.burrow.stateDir}.d = { mode = "0755"; user = "root"; group = "root"; };
        ${dir}.d = { mode = "0755"; inherit user; group = config.users.users.${user}.group; };
      };
    })
  ];
}
