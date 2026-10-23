# Options every burrow module reads: who the agents run as, where the projects
# are, where this machine's own flake is checked out. Imported by path from
# every module, so it's only ever in once.
{ config, lib, ... }:
let
  inherit (lib) mkOption types;
in
{
  options.burrow = {
    user = mkOption {
      type = types.str;
      default = "dev";
      description = ''
        The account you and your agents work as. It needs passwordless sudo
        (wheel with `security.sudo.wheelNeedsPassword = false`): undo, not
        prompts, is what keeps it safe.
      '';
    };

    projectsDir = mkOption {
      type = types.str;
      default = "/data/dev/src";
      description = ''
        One folder per project. Each gets an always-on Remote Control server
        (remote-control), jobs run in them (proj), and sandboxes clone them
        (agents), so for those it has to live on burrow.agents.dataDataset.
      '';
    };

    flake = mkOption {
      type = types.str;
      default = "${config.burrow.projectsDir}/${config.networking.hostName}";
      defaultText = lib.literalExpression ''"''${config.burrow.projectsDir}/''${config.networking.hostName}"'';
      description = ''
        The checkout of this machine's own flake. Its Remote Control session is
        the operator: the one you talk to about the machine itself.
      '';
    };

    owner = mkOption {
      type = types.str;
      default = "the owner";
      example = "Sam";
      description = "Who the agents answer to, as the machine's CLAUDE.md names them.";
    };

    undo.datasets = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "rpool/safe/home" "rpool/safe/data" ];
      description = ''
        ZFS datasets the Claude guard snapshots before anything deletes or
        rewrites files (see modules/undo.nix). Empty: no snapshots.
      '';
    };
  };
}
