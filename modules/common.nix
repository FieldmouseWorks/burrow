# Options every burrow module reads: who the agents run as, where the projects
# are, where this machine's own flake is checked out, the Claude Code build and
# the machine CLAUDE.md and MCP servers (claude/mcp.nix) the modules give it.
# Every module imports it; the key keeps it in only once.
inputs:
{
  key = "burrow/common";
  imports = [
    ./claude/mcp.nix
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

          prefix = mkOption {
            type = types.str;
            default = config.networking.hostName;
            defaultText = lib.literalExpression "config.networking.hostName";
            description = ''
              Names the machine's own commands and their units: PREFIX-switch,
              PREFIX-confirm, PREFIX-boot, PREFIX-update, PREFIX-health, PREFIX-notify.
            '';
          };

          stateDir = mkOption {
            type = types.str;
            default = "/var/lib/burrow";
            description = ''
              Where the switch, trial boots, notices and the update pipeline keep
              what must survive a reboot. Persist it if / is ephemeral.
            '';
          };

          scratchDir = mkOption {
            type = types.str;
            default = "${dirOf config.burrow.projectsDir}/scratch";
            defaultText = lib.literalExpression ''"''${dirOf config.burrow.projectsDir}/scratch"'';
            description = ''
              Throwaway clones and worktrees (the update pipeline's own clone, the
              fix jobs' worktrees). Outside projectsDir, so they get no Remote
              Control server.
            '';
          };

          tools = mkOption {
            type = types.attrsOf types.package;
            default = { };
            internal = true;
            description = "The commands each module builds, for the others to call.";
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

          claude.manifest = mkOption {
            type = types.nullOr types.path;
            default = null;
            example = lib.literalExpression "./claude-code.json";
            description = ''
              A Claude Code release manifest (from downloads.claude.ai's release
              channel) to build instead of the version nixpkgs-unstable carries.
              burrow.updates keeps it at the newest release.
            '';
          };

          claude.package = mkOption {
            type = types.package;
            default = import ./claude/package.nix {
              inherit inputs;
              inherit (config.burrow.claude) manifest;
              system = pkgs.stdenv.hostPlatform.system;
            };
            defaultText = lib.literalMD "Claude Code from burrow's nixpkgs-unstable";
            description = "The Claude Code build, here and in the agent sandboxes.";
          };

          claude.memory = {
            high = mkOption {
              type = types.str;
              default = "40%";
              example = "24G";
              description = ''
                Where every Claude this machine runs on its own (proj jobs, Remote
                Control servers and their sessions) starts being slowed and pushed
                to give memory back: claude.slice's MemoryHigh, a size or a share
                of the machine's memory.
              '';
            };
            max = mkOption {
              type = types.str;
              default = "50%";
              example = "30G";
              description = ''
                The most they may hold between them (claude.slice's MemoryMax), none
                of it in swap: swap would let them grow past it without ever reaching
                it. Past it the kernel kills a Claude, proj jobs first, rather than
                anything else on the machine: CI, services, the ZFS ARC. Leave room
                for those.
              '';
            };
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

        # tmux for the Claudes that run in one (Remote Control servers, the
        # usage watcher's Claude Code). Not tmux's usual build: that one moves
        # each pane into a scope of its own, which takes a server and its
        # sessions out of their unit and out from under its cap.
        config.burrow.tools.tmux = pkgs.tmux.overrideAttrs (old: {
          configureFlags = old.configureFlags ++ [ "--disable-cgroups" ];
        });

        # In the user's systemd manager, where proj jobs (claude-jobs.slice)
        # and Remote Control servers (claude-rc.slice) run. Many headless
        # Claudes at a few GB each once ran a machine out of memory and the
        # kernel killed a CI run instead of one of them; capped here, they
        # can only run out of their own. Not into swap either: what they
        # swap is past the cap and still costs the machine, zram's in RAM and
        # a disk's in I/O. One whose caps allowed it thrashed until it hung,
        # with the kernel killing nothing.
        config.systemd.user.slices.claude = {
          description = "Every Claude that runs on its own";
          sliceConfig = {
            MemoryHigh = config.burrow.claude.memory.high;
            MemoryMax = config.burrow.claude.memory.max;
            MemorySwapMax = 0;
          };
        };

        # The git the machine and burrow's units run. curl 8.23 strips the
        # credentials from the URL it reports, and git 2.54 takes that for a
        # redirect and forgets them, so a clone with a token-only URL
        # (Renovate's) fails. Drop the patch once nixpkgs' git has it (it then
        # stops applying). Here and not an overlay: that would rebuild all
        # that builds with git.
        config.programs.git.package = lib.mkDefault (pkgs.git.overrideAttrs (old: {
          patches = (old.patches or [ ]) ++ [ ../patches/git-curl-effective-url.patch ];
        }));
      })
  ];
}
