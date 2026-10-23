# HOST-notify: how the machine tells its owner something they must know, as
# one GitHub issue per problem (GitHub's app puts it on their phone), closed by
# itself when the problem clears (notify/notify.sh). With no repo, notices go
# to the journal only. The trial boots and the update pipeline use it.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.notify;
  inherit (config.burrow) user;
  name = "${config.burrow.prefix}-notify";

  vars = {
    NOTIFY = name;
    REPO = if cfg.repo == null then "" else cfg.repo;
    ASSIGNEE = if cfg.assignee == null then "" else cfg.assignee;
    LABEL = cfg.label;
    DIR = "${config.burrow.stateDir}/notices";
    BURROW_USER = user;
    HOME_DIR = config.users.users.${user}.home;
  };

  notify = pkgs.writeShellApplication {
    inherit name;
    runtimeInputs = [ pkgs.coreutils pkgs.util-linux pkgs.systemd pkgs.gh ];
    text = lib.concatStrings (lib.mapAttrsToList (n: v: "${n}=${lib.escapeShellArg v}\n") vars)
      + builtins.readFile ./notify/notify.sh;
  };
in
{
  imports = [ (import ./common.nix inputs) ];

  options.burrow.notify = {
    enable = lib.mkEnableOption "HOST-notify, notices to the owner as GitHub issues";

    repo = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "sam/homelab";
      description = ''
        The GitHub repo (OWNER/NAME) the issues go to, opened with the gh login
        of burrow.user. null: notices only go to the journal.
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
      description = "The label every notice carries; it's how the script finds its own issues.";
    };
  };

  config = lib.mkMerge [
    # Always built, so the modules that call it work whether or not it's on PATH.
    { burrow.tools.notify = notify; }

    (lib.mkIf cfg.enable {
      environment.systemPackages = [ notify ];
      systemd.tmpfiles.settings.burrow = {
        ${config.burrow.stateDir}.d = { mode = "0755"; user = "root"; group = "root"; };
        ${vars.DIR}.d = { mode = "0755"; inherit user; group = config.users.users.${user}.group; };
      };
    })
  ];
}
