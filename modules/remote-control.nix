# Remote Control servers, one per project, always on. This is how you reach the
# box from your phone (Claude app) and any browser (claude.ai/code): each
# project shows up as its own machine/folder there.
#
# Every project in burrow.projectsDir gets one, found by itself: a git checkout
# with an origin. Clone a repo there and it appears on the phone a moment
# later; delete the folder and its server stops. A folder that isn't a project
# (notes, scratch, a folder of a project's extra checkouts, a linked worktree)
# gets none. The machine's own flake (burrow.flake) always gets one, so its
# session is the operator: the one you talk to about the machine itself.
#
# Each server runs `claude remote-control` in its own tmux server, so it has
# the terminal it wants and you can look at it: `tmux -L rc-NAME attach`.
#
# Nothing about a server needs a hand at its terminal: claude-rc-serve
# (remote-control/serve.sh) trusts the folder (claude-trust) before Claude
# would ask. What's left is a one-time `claude remote-control` run by hand on a
# new machine, to log in and say yes to Remote Control itself.
#
# This is also how a claude.ai project reaches a project: a thread there starts
# a session on that project's server and talks to it (docs/front-door.md).
# Those sessions each get their own git worktree (burrow.remoteControl.spawn),
# and run in burrow.remoteControl.permissionMode: bypassPermissions alongside
# burrow.claude, like every other session here, so only its one-way doors ask.
#
# The servers keep memory back when the machine runs short
# (burrow.remoteControl.memoryLow): the kernel takes it from proj's jobs, CI
# and builds first. Each server and every session it holds may grow to half
# the machine between them (burrow.remoteControl.memoryMax), a backstop for a
# runaway: past it the kernel kills the biggest process there, the session
# that grew or a build it ran, and only that. The server and its other
# sessions carry on, and a killed session's transcript stays on disk for
# claude.ai to resume.
inputs:
{ config, lib, pkgs, utils, ... }:
let
  cfg = config.burrow.remoteControl;
  inherit (config.burrow) user projectsDir;
  tmux = "${cfg.tmux}/bin/tmux";
  # How long a stale server waits on a session that has stopped doing anything.
  idleMinutes = 30;

  serve = pkgs.writeShellApplication {
    name = "claude-rc-serve";
    runtimeInputs = [ pkgs.coreutils config.burrow.tools.claude-trust ];
    text = ''
      PROJECTS=${lib.escapeShellArg projectsDir}
      FLAKE=${lib.escapeShellArg config.burrow.flake}
      SPAWN=${lib.escapeShellArg cfg.spawn}
      SAME_DIR=(${lib.escapeShellArgs cfg.sameDir})
      MODE=${lib.escapeShellArg (if cfg.permissionMode == null then "" else cfg.permissionMode)}
      CLAUDE=${config.burrow.claude.package}/bin/claude
      IDLE_SESSION=${if cfg.idleSession then "1" else "0"}
    '' + builtins.readFile ./remote-control/serve.sh;
  };

  # Stop a server the way Ctrl-C would, so it takes its registration with it:
  # killed outright, the folder stays "already served" at claude.ai for a few
  # minutes and the restart fails until then.
  stop = pkgs.writeShellApplication {
    name = "claude-rc-stop";
    runtimeInputs = [ cfg.tmux pkgs.coreutils ];
    text = ''
      sock=rc-''${1:?usage: claude-rc-stop NAME}
      if pid=$(tmux -L "$sock" display-message -p -t rc '#{pane_pid}' 2>/dev/null); then
        kill -INT "$pid" 2>/dev/null || true
        for _ in $(seq 100); do
          kill -0 "$pid" 2>/dev/null || break
          sleep 0.1
        done
      fi
      tmux -L "$sock" kill-server 2>/dev/null || true
    '';
  };

  # claude-rc-scan (crates/burrow-rc-scan, whose lib.rs says what it does and
  # how it joins the units below), with its config built in. A unit's Exec has
  # systemd's bare PATH, so it runs what it needs by these paths.
  scan = pkgs.runCommand "claude-rc-scan" {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = "claude-rc-scan";
  } ''
    makeBinaryWrapper ${inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-rc-scan}/bin/burrow-rc-scan \
      $out/bin/claude-rc-scan --add-flags ${scanConfig}
  '';
  scanConfig = pkgs.writeText "claude-rc-scan.json" (builtins.toJSON {
    inherit projectsDir idleMinutes;
    flake = config.burrow.flake;
    inherit tmux;
    serve = "${serve}/bin/claude-rc-serve";
    tools = {
      systemctl = "${config.systemd.package}/bin/systemctl";
      systemdRun = "${config.systemd.package}/bin/systemd-run";
      systemdEscape = "${config.systemd.package}/bin/systemd-escape";
      git = lib.getExe config.programs.git.package;
      jq = lib.getExe pkgs.jq;
    };
  });
in
{
  imports = [ (import ./common.nix inputs) ];

  options.burrow.remoteControl = {
    enable = lib.mkEnableOption "an always-on Claude Remote Control server for every project folder";
    spawn = lib.mkOption {
      type = lib.types.enum [ "worktree" "same-dir" ];
      default = "worktree";
      description = ''
        Where a session started from claude.ai works. worktree: its own git
        worktree of the project, so sessions working at once never share a
        checkout (an idleSession stays in the folder itself). same-dir: all
        of them in the folder. The machine's own flake is always same-dir.
      '';
    };
    permissionMode = lib.mkOption {
      type = lib.types.nullOr (lib.types.enum [ "acceptEdits" "auto" "bypassPermissions" "default" "dontAsk" "plan" ]);
      default = if config.burrow.claude.enable or false then "bypassPermissions" else null;
      defaultText = lib.literalExpression ''if config.burrow.claude.enable then "bypassPermissions" else null'';
      description = ''
        The permission mode for sessions started from claude.ai. Without it,
        the mode claude.ai asks for wins over the machine's managed defaultMode.
        bypassPermissions keeps burrow.claude's ask rules: its one-way doors
        still ask. null: whatever claude.ai and the settings say.
      '';
    };
    idleSession = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Keep one session open in each project's folder, ready before anyone
        asks for it (Claude's own default). Each is a whole Claude holding a
        few GB whether anyone uses it or not; one per project ran a machine
        out of memory. Off: sessions start when claude.ai asks for one, in
        the project's spawn mode.
      '';
    };
    sameDir = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "website" ];
      description = "Project folders whose sessions all work in the folder itself, whatever spawn says.";
    };
    memoryMax = lib.mkOption {
      type = lib.types.str;
      default = "50%";
      example = "12G";
      description = ''
        The most memory one server may hold with all its sessions (its unit's
        MemoryMax, a size or a share of the machine's memory; "infinity" for
        none), none of it in swap: a backstop for a runaway. systemd-oomd
        never picks a server (burrow.ops.oomd) and the kernel kills proj's
        jobs first, so without it one runaway session would take every job
        down before itself. Past it the kernel kills the
        biggest process there, the session that grew or a build or
        evaluation it ran, and the server and its other sessions carry on
        (OOMPolicy=continue). No swap, so it's a cap that's reached, and so
        the machine's swap watch, which can't see a server's opt-out of
        oomd, never picks one.
      '';
    };
    memoryLow = lib.mkOption {
      type = lib.types.str;
      default = "10%";
      example = "6G";
      description = ''
        Memory the servers and their sessions keep between them when the
        machine runs short (MemoryLow, a size or a share of the machine's
        memory): the kernel takes it from everything else first, proj's
        jobs, CI and builds among them. It costs nothing while there's room.
        Set on claude-rc.slice and on every slice above it, since a cgroup
        keeps no more than its parent does; what the servers don't use of
        it, the jobs beside them in claude.slice may. 10% is about what all
        of remi's servers held on a busy evening (6.1G of 62.8G).
      '';
    };
    servers = lib.mkOption {
      type = lib.types.attrsOf (lib.types.submodule {
        options.memoryMax = lib.mkOption {
          type = lib.types.str;
          default = cfg.memoryMax;
          defaultText = lib.literalExpression "config.burrow.remoteControl.memoryMax";
          description = "This server's memoryMax.";
        };
      });
      default = { };
      example = lib.literalExpression ''{ homelab.memoryMax = "12G"; }'';
      description = "Settings for the servers of some project folders, by folder name.";
    };
    tmux = lib.mkOption {
      type = lib.types.package;
      default = config.burrow.tools.tmux;
      defaultText = lib.literalMD "tmux, built with `--disable-cgroups` (common.nix)";
      internal = true;
      description = ''
        The tmux the servers run in, one that keeps their sessions under
        their unit's cap. The memory test starts its servers on tmux's
        usual build, to see them catch up.
      '';
    };
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [ {
    systemd.user.services."claude-rc@" = {
      description = "Claude Remote Control for ${projectsDir}/%I";
      after = [ "network-online.target" ];
      # A switch must not kill the operator that has to confirm it; claude-rc-scan
      # restarts a server onto the new config once it's idle.
      restartIfChanged = false;
      unitConfig = {
        ConditionUser = user;
        ConditionPathIsDirectory = "${projectsDir}/%I";
      };
      serviceConfig = {
        Type = "forking";
        # A login shell, so sessions get the same PATH and env as an SSH login.
        # The name goes in as an argument, not spliced into the command string,
        # so a folder with a space in it ("agent pipeline") still works.
        ExecStart = "${tmux} -L rc-%I new-session -d -s rc -c ${projectsDir}/%I ${pkgs.bashInteractive}/bin/bash -lc 'exec ${serve}/bin/claude-rc-serve \"$$1\"' rc %I";
        ExecStop = "${stop}/bin/claude-rc-stop %I";
        Restart = "always";
        RestartSec = 15;
        # Beside the proj jobs in claude.slice (common.nix), and keeping
        # memory back from them (memoryLow).
        Slice = "claude-rc.slice";
        # The backstop, for the server and its sessions together (memoryMax).
        MemoryMax = cfg.memoryMax;
        MemorySwapMax = 0;
        OOMPolicy = "continue";
        # systemd-oomd kills whole units, here every session at once.
        ManagedOOMPreference = "omit";
      };
    };

    # What the servers keep back (memoryLow), on their slice and each one
    # above it: in the user's manager, and in the system's from the user's
    # manager up. Every user's slice and manager gets it, not just
    # burrow.user's; one using less keeps only what it uses.
    systemd.user.slices.claude.sliceConfig.MemoryLow = cfg.memoryLow;
    systemd.user.slices.claude-rc = {
      description = "Remote Control servers";
      sliceConfig.MemoryLow = cfg.memoryLow;
    };
    systemd.services."user@".serviceConfig.MemoryLow = cfg.memoryLow;
    # systemd has no user-.slice, only the drop-ins every user-UID.slice
    # takes: a unit of that name would configure none of them.
    systemd.slices."user-" = {
      overrideStrategy = "asDropin";
      sliceConfig.MemoryLow = cfg.memoryLow;
    };
    systemd.slices.user.sliceConfig.MemoryLow = cfg.memoryLow;

    systemd.user.services.claude-rc-scan = {
      description = "Start a Remote Control server for each project folder";
      # On boot, and on every switch, which makes servers stale (a new
      # claude-rc-serve) and leaves them running: it starts default.target
      # again once it has re-executed the user manager.
      wantedBy = [ "default.target" ];
      unitConfig = {
        ConditionUser = user;
        # Its path units start it on every change, and changes coming one
        # after another (many sessions at once) would hit the default start
        # limit, which fails the path units too and leaves nothing watching.
        # A start while one runs only joins it, so there's nothing to limit.
        StartLimitIntervalSec = 0;
      };
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${scan}/bin/claude-rc-scan";
        # Failed or not.
        ExecStopPost = "${scan}/bin/claude-rc-scan done";
      };
    };

    # The look after scan %i, once it has stopped (see claude-rc-scan).
    systemd.user.services."claude-rc-again@" = {
      description = "Scan again for what changed while a scan ran";
      after = [ "claude-rc-scan.service" ];
      unitConfig.ConditionUser = user;
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${scan}/bin/claude-rc-scan again %i";
      };
    };

    # A session's status changing (working, done, gone) may free its stale
    # server, or move the end of its quiet spell.
    systemd.user.paths.claude-rc-scan-sessions = {
      wantedBy = [ "default.target" ];
      unitConfig.ConditionUser = user;
      pathConfig = {
        PathChanged = "%h/.claude/sessions";
        Unit = "claude-rc-scan.service";
      };
    };

    # A folder that isn't a project yet becoming one where it stands: its
    # .git/config written (see claude-rc-scan, which starts and stops these).
    # Watched before it exists, from the closest folder that does.
    systemd.user.paths."claude-rc-origin@" = {
      description = "Scan when ${projectsDir}/%I becomes a git checkout with an origin";
      unitConfig.ConditionUser = user;
      pathConfig = {
        PathChanged = "${projectsDir}/%I/.git/config";
        Unit = "claude-rc-scan.service";
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

    # The servers live in the user's systemd manager, which has to run from
    # boot on, logged in or not.
    users.users.${user}.linger = lib.mkDefault true;

    systemd.tmpfiles.rules = [ "d ${projectsDir} 0755 ${user} ${config.users.users.${user}.group} -" ];

    # Claude Code too, for when burrow.claude (which also installs it) is off.
    environment.systemPackages = [ pkgs.tmux config.burrow.claude.package ];

    burrow.claude.machineMd = lib.mkOrder 690 ''
      ## Sessions started from a claude.ai project

      ${config.burrow.owner} runs every project from one claude.ai project chat. When a thread
      there needs work done in a project, it starts a Remote Control session on that
      project's server, and that's you: the project's own Claude, with its checkout,
      CLAUDE.md, tools and logins. The thread is where ${config.burrow.owner} talks to you,
      usually from a phone, and what they type there reaches you.

      - You're in your own git worktree of the project, so other sessions can work
        at the same time. Branch, commit, push and open the PR from there, and
        leave the main checkout in ${projectsDir}/NAME alone.
      - Send results, PR links and questions back to the thread as you go, short
        and in plain words. A question waits for ${config.burrow.owner}'s answer in the
        thread; carry on with whatever doesn't depend on it.
      - Long work is fine. Keep going until it's done, then say so.
    '';
  } {
    # A server's own settings, in a drop-in for its instance: the unit name
    # claude-rc-scan starts, which systemd-escape gives it. escapeSystemdPath
    # writes the same for printable ASCII only.
    assertions = lib.mapAttrsToList (name: _: {
      assertion = builtins.match "[ -~]+" name != null;
      message = "burrow.remoteControl.servers.\"${name}\": only a folder named in printable ASCII can have settings of its own.";
    }) cfg.servers;
    systemd.user.services = lib.mapAttrs' (name: server:
      lib.nameValuePair "claude-rc@${utils.escapeSystemdPath name}" {
        overrideStrategy = "asDropin";
        serviceConfig.MemoryMax = server.memoryMax;
      }) cfg.servers;
  } ]);
}
