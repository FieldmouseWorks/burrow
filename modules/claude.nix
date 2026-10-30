# Claude Code, configured once, system-wide, so no project ever asks about
# permissions again. Everything runs in bypassPermissions; the only prompts are
# the one-way doors (R2) listed below. Also writes /etc/claude-code/CLAUDE.md,
# which every Claude session on the box reads: each burrow module adds its
# section to burrow.claude.machineMd.
inputs:
{ pkgs, lib, config, ... }:
let
  cfg = config.burrow.claude;
  inherit (config.networking) hostName;

  guard = pkgs.writeShellApplication {
    name = "claude-guard";
    runtimeInputs = [ pkgs.jq pkgs.gnugrep pkgs.coreutils config.programs.git.package config.boot.zfs.package ];
    text = ''
      GUARD_SNAPSHOT_DATASETS=(${lib.escapeShellArgs (lib.optionals (config.burrow.undo.enable or false) config.burrow.undo.datasets)})
    '' + builtins.readFile ./claude/guard.sh;
  };

  # One rule per command, with and without sudo.
  both = cmds: lib.concatMap (c: [ "Bash(${c})" "Bash(sudo ${c})" ]) cmds;

  r2 = both [
    # Storage: destroys data with no undo.
    "zfs destroy *"
    "zfs rollback *"
    "zpool destroy *"
    "zpool labelclear *"
    "wipefs *"
    "mkfs*"
    "sgdisk *"
    "blkdiscard *"
    "dd *"
    "nixos-anywhere *"
    "disko *"
    # Firmware: a bad flash can brick the board, and there's no console to fix it.
    "fwupdmgr install *"
    "fwupdmgr update*"
    "fwupdmgr upgrade*"
    "fwupdmgr reinstall *"
    "fwupdmgr downgrade *"
    "flashrom *"
    "nvme fw-download *"
    "nvme fw-commit *"
    "nvme fw-activate *"
    "hdparm --fwdownload*"
    # Erases a whole drive in firmware, no snapshot can catch it.
    "nvme format *"
    "nvme sanitize *"
    "hdparm --security-erase*"
    "hdparm --security-erase-enhanced*"
    # Remote data with no undo.
    "rclone delete *"
    "rclone deletefile *"
    "rclone purge *"
    "rclone rmdir *"
    "rclone rmdirs *"
    "rclone sync *"
    "gh repo delete *"
    "gh release delete *"
    "git push --force origin main*"
    "git push -f origin main*"
    "git push --force origin master*"
    "git push -f origin master*"
  ] ++ both cfg.extraAsk;

  managed = lib.recursiveUpdate {
    permissions = {
      defaultMode = "bypassPermissions";
      ask = r2;
    };
    skipDangerousModePermissionPrompt = true;
    env = {
      # Updates arrive through the machine's flake, never in place.
      DISABLE_AUTOUPDATER = "1";
    };
    hooks.PreToolUse = [
      {
        matcher = "Bash";
        hooks = [
          {
            type = "command";
            command = "${guard}/bin/claude-guard";
            timeout = 10;
          }
        ];
      }
    ];
  } cfg.extraSettings;

  robot = config.burrow.robot.enable or false;
in
{
  imports = [ (import ./common.nix inputs) ];

  options.burrow.claude = {
    enable = lib.mkEnableOption "system-wide Claude Code with reversibility-classed permissions";

    extraAsk = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "terraform destroy *" ];
      description = "More one-way-door commands that should ask (each also with sudo).";
    };

    extraSettings = lib.mkOption {
      type = lib.types.attrs;
      default = { };
      description = "Merged into /etc/claude-code/managed-settings.json.";
    };

    boxNotes = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = ''
        Extra bullets for "How the box works" in the machine CLAUDE.md, before
        the ones burrow writes (markdown, continuation lines indented by two).
      '';
    };

    standingAuthority = lib.mkOption {
      type = lib.types.lines;
      default = "";
      description = ''
        What the owner has approved in advance, as markdown for its own section
        of the machine CLAUDE.md ("Standing authority", just before "What still
        asks"), so no session asks them to approve it again. Empty: no section.
      '';
    };

  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    environment.etc."claude-code/managed-settings.json".text = builtins.toJSON managed;
    environment.etc."claude-code/CLAUDE.md".text = cfg.machineMd;

    # Logins and history live in ~/.claude.

    burrow.claude.machineMd = lib.mkMerge [
      (lib.mkOrder 600 (''
        ## How the box works

      '' + lib.concatMapStrings (n: "${n}\n") (cfg.boxNotes ++ [
        ''
          - You are `${config.burrow.user}`, with sudo and no password. Use it when you need it.''
      ] ++ lib.optional (config.burrow.remoteControl.enable or false) ''
          - Projects live in ${config.burrow.projectsDir}. Each folder gets its own always-on Remote
            Control session automatically.'')))

      (lib.mkOrder 1000 ''
        ## When a tool gets in the way: patch it

        ${hostName} is built from source, so fix the tool before working around it. When an
        open-source piece of the stack misbehaves for agents (prompts, needs a TTY,
        bad defaults, unparseable output), patch it with a Nix overlay in the flake
        (`nixpkgs.overlays`, patch files under `patches/`), switch, and send the patch
        upstream when it's useful beyond ${hostName}. Don't build a wrapper, shim or
        retry loop around something you could fix.

        Claude Code is the exception: it's closed source, so it gets wrapped and
        configured (burrow's modules/claude.nix), not patched.
      '')

      (lib.mkIf (cfg.standingAuthority != "") (lib.mkOrder 1150 ''
        ## Standing authority

        ${cfg.standingAuthority}''))

      (lib.mkOrder 1200 ''
        ## What still asks ${config.burrow.owner}

        Only one-way doors: destroying or rolling back ZFS datasets, wiping or
        partitioning disks, deleting remote data (rclone delete/purge/sync, gh repo
        delete), force-pushing a default branch, flashing firmware${lib.optionalString robot ", and Hetzner Robot\ncalls that cancel or reinstall the server"}. Everything else: just do it, then
        say what you did.${lib.optionalString (cfg.standingAuthority != "") "\nThe standing authority above is already given: never ask ${config.burrow.owner} to restate it."}
      '')
    ];
  };
}
