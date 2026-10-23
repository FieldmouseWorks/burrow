# Remote Control servers, one per project, always on. This is how you reach the
# box from your phone (Claude app) and any browser (claude.ai/code): each
# project shows up as its own machine/folder there.
#
# Every folder in burrow.projectsDir gets one, found by itself: clone a repo
# there and it appears on the phone a moment later; delete the folder and its
# server stops. The machine's own flake (burrow.flake) is one of them, so its
# session is the operator: the one you talk to about the machine itself.
#
# Each server runs `claude remote-control` in its own tmux server, so it has
# the terminal it wants and you can look at it: `tmux -L rc-NAME attach`.
#
# The first start of each one stops at two questions (trust the folder, enable
# Remote Control), so answer them once per project, by hand:
#
#   systemctl --user stop claude-rc@NAME
#   cd PROJECTS/NAME && claude remote-control --name NAME --spawn=same-dir
#   # y (trust), y (enable); once it's connected, Ctrl-C, then:
#   systemctl --user start claude-rc@NAME
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.remoteControl;
  inherit (config.burrow) user projectsDir;
  tmux = "${pkgs.tmux}/bin/tmux";

  scan = pkgs.writeShellApplication {
    name = "claude-rc-scan";
    runtimeInputs = [ pkgs.systemd pkgs.coreutils ];
    text = ''
      # Start a server for every project folder, stop the ones whose folder is gone.
      want=()
      for d in ${lib.escapeShellArg projectsDir}/*/; do
        [ -d "$d" ] || continue
        want+=("$(systemd-escape --template claude-rc@.service "$(basename "$d")")")
      done
      for unit in "''${want[@]}"; do
        systemctl --user start --no-block "$unit"
      done
      while read -r unit _; do
        keep=
        for w in "''${want[@]}"; do [ "$w" = "$unit" ] && keep=1; done
        [ -n "$keep" ] || systemctl --user stop --no-block "$unit"
      done < <(systemctl --user list-units --plain --no-legend --all 'claude-rc@*.service')
    '';
  };
in
{
  imports = [ ./common.nix ];

  options.burrow.remoteControl.enable =
    lib.mkEnableOption "an always-on Claude Remote Control server for every project folder";

  config = lib.mkIf cfg.enable {
    systemd.user.services."claude-rc@" = {
      description = "Claude Remote Control for ${projectsDir}/%I";
      after = [ "network-online.target" ];
      # A switch must not kill the operator that has to confirm it.
      restartIfChanged = false;
      unitConfig = {
        ConditionUser = user;
        ConditionPathIsDirectory = "${projectsDir}/%I";
      };
      serviceConfig = {
        Type = "forking";
        # A login shell, so sessions get the same PATH and env as an SSH login.
        # --spawn is set so the first start doesn't stop on a question. The other
        # two (trust the folder, enable Remote Control) still need one manual run
        # per project (see the top of this file). The name goes in as an argument,
        # not spliced into the command string, so a folder with a space in it
        # ("agent pipeline") still works.
        ExecStart = "${tmux} -L rc-%I new-session -d -s rc -c ${projectsDir}/%I ${pkgs.bashInteractive}/bin/bash -lc 'exec claude remote-control --name \"$$1\" --spawn=same-dir' rc %I";
        ExecStop = "${tmux} -L rc-%I kill-server";
        Restart = "always";
        RestartSec = 15;
      };
    };

    systemd.user.services.claude-rc-scan = {
      description = "Start a Remote Control server for each project folder";
      wantedBy = [ "default.target" ];
      unitConfig.ConditionUser = user;
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${scan}/bin/claude-rc-scan";
      };
    };

    # Rescan whenever a folder is added to or removed from the projects folder.
    systemd.user.paths.claude-rc-scan = {
      wantedBy = [ "default.target" ];
      unitConfig.ConditionUser = user;
      pathConfig = {
        PathChanged = projectsDir;
        Unit = "claude-rc-scan.service";
      };
    };

    systemd.tmpfiles.rules = [ "d ${projectsDir} 0755 ${user} ${config.users.users.${user}.group} -" ];

    environment.systemPackages = [ pkgs.tmux ];
  };
}
