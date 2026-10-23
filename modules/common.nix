# Options every burrow module reads: who the agents run as, where the projects
# are, where this machine's own flake is checked out, the Claude Code build and
# the machine CLAUDE.md the modules write into. Every module imports it; the
# key keeps it in only once.
inputs:
{
  key = "burrow/common";
  imports = [
    ({ config, lib, pkgs, ... }:
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

          claude.package = mkOption {
            type = types.package;
            default = import ./claude/package.nix {
              inherit inputs;
              system = pkgs.stdenv.hostPlatform.system;
            };
            defaultText = lib.literalMD "Claude Code from burrow's nixpkgs-unstable";
            description = "The Claude Code build, here and in the agent sandboxes.";
          };

          claude.machineMd = mkOption {
            type = types.lines;
            default = "";
            description = ''
              /etc/claude-code/CLAUDE.md (written when burrow.claude is enabled).
              burrow's modules fill in their sections at lib.mkOrder 600 to 1400;
              put an intro in lib.mkBefore (500) and an ending in lib.mkAfter (1500).
            '';
          };
        };
      })
  ];
}
