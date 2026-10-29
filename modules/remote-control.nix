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
# Nothing about a server needs a hand at its terminal: claude-rc-serve
# (remote-control/serve.sh) trusts the folder before Claude would ask. What's
# left is a one-time `claude remote-control` run by hand on a new machine, to
# log in and say yes to Remote Control itself.
#
# This is also how a claude.ai project reaches a project: a thread there starts
# a session on that project's server and talks to it (docs/front-door.md).
# Those sessions each get their own git worktree (burrow.remoteControl.spawn),
# and run in burrow.remoteControl.permissionMode: bypassPermissions alongside
# burrow.claude, like every other session here, so only its one-way doors ask.
#
# Each server and every session it holds share one memory cap
# (burrow.remoteControl.memoryMax). Past it the kernel kills the biggest
# process there, usually the session that grew, and only that: the server and
# its other sessions carry on, and the killed one's transcript stays on disk
# for claude.ai to resume.
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
    runtimeInputs = [ pkgs.coreutils pkgs.findutils pkgs.jq ];
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

  scan = pkgs.writeShellApplication {
    name = "claude-rc-scan";
    runtimeInputs = [ pkgs.systemd pkgs.coreutils pkgs.findutils pkgs.gnugrep pkgs.procps pkgs.jq ];
    text = ''
      # Start a server for every project folder, stop the ones whose folder is gone,
      # and restart the stale ones once they hold no session.
      #
      # A start while a scan runs (a path unit's, the booked timer's) only
      # joins it, until it has stopped, so what changed meanwhile would go
      # unseen: a session's status, a folder added or gone, a quiet spell's
      # end. So each scan notes what it saw as it began and when its spells
      # end, and as it stops (`claude-rc-scan done`) asks for a look after it
      # (claude-rc-again@ID, `claude-rc-scan again ID`), which waits for it to
      # be stopped and scans again if anything changed since or a spell is
      # over. That look has a unit of its own, so no start joins it; and its
      # start comes once this scan's is over, so nothing is lost after it
      # either: a scan already running then has a look of its own to come.
      sessions=$HOME/.claude/sessions
      notes=$XDG_RUNTIME_DIR/claude-rc-scan
      seen() {
        { find "$sessions" -mindepth 1 -maxdepth 1 -printf '%p %T@\n' 2>/dev/null || true; } | sort
        { find ${lib.escapeShellArg projectsDir} -mindepth 1 -maxdepth 1 -printf '%p\n' 2>/dev/null || true; } | sort
      }
      case ''${1:-} in
        done)
          exec systemctl --user start --no-block "claude-rc-again@$INVOCATION_ID.service"
          ;;
        again)
          note=$notes/$2
          wake=$(cat "$note.wake" 2>/dev/null || true)
          if [ "$(seen)" != "$(cat "$note.seen" 2>/dev/null || true)" ] ||
            { [ -n "$wake" ] && (( wake <= $(date +%s%3N) )); }; then
            systemctl --user start --no-block claude-rc-scan.service
          fi
          rm -f "$note.seen" "$note.wake"
          exit 0
          ;;
      esac
      mkdir -p "$notes"
      note=$notes/$INVOCATION_ID
      seen >"$note.seen"
      # A linked git worktree (its .git is a file) is a checkout of a project
      # that already has its own server, so it gets none; neither does a folder
      # matching burrow.remoteControl.skip.
      skips=(${lib.escapeShellArgs cfg.skip})
      want=()
      for d in ${lib.escapeShellArg projectsDir}/*/; do
        [ -d "$d" ] || continue
        name=$(basename "$d")
        [ -f "$d/.git" ] && continue
        skip=
        for glob in "''${skips[@]}"; do
          # shellcheck disable=SC2053 # the glob is meant to match
          [[ $name == $glob ]] && skip=1
        done
        [ -z "$skip" ] || continue
        want+=("$(systemd-escape --template claude-rc@.service "$name")")
      done
      for unit in "''${want[@]}"; do
        systemctl --user start --no-block "$unit"
      done
      while read -r unit _; do
        keep=
        for w in "''${want[@]}"; do [ "$w" = "$unit" ] && keep=1; done
        [ -n "$keep" ] || systemctl --user stop --no-block "$unit"
      done < <(systemctl --user list-units --plain --no-legend --all 'claude-rc@*.service')
      # A switch leaves running servers alone (restartIfChanged), so a server
      # keeps the tmux and the claude-rc-serve it started with: its Claude
      # Code, its permission mode, where its sessions run. One that runs any
      # other is stale, and restarts here once none of its sessions is in
      # use. A session (a child of its Claude) stays open as long as
      # claude.ai keeps it, so being open is not being in use: Claude Code's
      # ~/.claude/sessions/PID.json says what it's doing, and only one that
      # is working, or did something in the last ${toString idleMinutes} minutes, holds the
      # server. Nobody is cut off mid-turn, and an open but idle session
      # doesn't keep a stale server forever. Its transcript stays on disk,
      # and claude.ai resumes it on the new one.
      #
      # A held server's scan comes again when what holds it changes: a
      # session's status (the path unit on ~/.claude/sessions), or the end of
      # its quiet spell, which this scan books (claude-rc-scan.timer), at the
      # latest of its sessions' and the soonest of the servers'. One that is
      # working, or says nothing, is looked at again a quiet spell from now:
      # it may end without a word.
      now=$(date +%s%3N)
      quiet=$(( ${toString idleMinutes} * 60000 ))
      # held SESSION: whether it holds its server; `until` says until when,
      # at the soonest.
      held() {
        local f=$sessions/$1.json status at
        until=$(( now + quiet ))
        [ -r "$f" ] || return 0
        status=$(jq -r '.status // ""' "$f") || return 0
        at=$(jq -r '.statusUpdatedAt // 0' "$f") || return 0
        [ "$status" = busy ] && return 0
        until=$(( at + quiet ))
        (( now < until ))
      }
      wake=
      for unit in "''${want[@]}"; do
        pid=$(systemctl --user show -p MainPID --value "$unit")
        [ "$pid" != 0 ] && [ -r "/proc/$pid/cmdline" ] || continue
        [[ $(tr '\0' ' ' <"/proc/$pid/cmdline") == ${lib.escapeShellArg "${tmux} "}*${lib.escapeShellArg "${serve}/bin/claude-rc-serve"}* ]] && continue
        busy=
        last=0
        for rc in $(pgrep -P "$pid"); do
          # Any state but Z: a zombie is a session that already ended.
          for s in $(pgrep -P "$rc" -r D,I,R,S,T,t); do
            if held "$s"; then
              busy=1
              (( until <= last )) || last=$until
            fi
          done
        done
        if [ -z "$busy" ]; then
          systemctl --user restart --no-block "$unit"
        elif [ -z "$wake" ] || (( last < wake )); then
          wake=$last
        fi
      done
      # The wake-up: a timer of its own for this unit, gone once it fires.
      systemctl --user stop claude-rc-scan.timer 2>/dev/null || true
      echo "$wake" >"$note.wake"
      if [ -n "$wake" ]; then
        # From the clock now, not the scan's start: a slow scan books no
        # later. In milliseconds: a timer never fires early, so the scan it
        # starts finds the spell over. A spell over already is the look
        # after this scan's to see.
        left=$(( wake - $(date +%s%3N) ))
        if (( left > 0 )); then
          systemd-run --user --quiet --unit=claude-rc-scan --on-active="''${left}ms" \
            --timer-property=AccuracySec=1s --timer-property=RemainAfterElapse=no
        fi
      fi
    '';
  };
in
{
  imports = [ (import ./common.nix inputs) ];

  options.burrow.remoteControl = {
    enable = lib.mkEnableOption "an always-on Claude Remote Control server for every project folder";
    skip = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ "*-worktrees" ];
      example = [ "*-worktrees" "scratch-*" ];
      description = ''
        Shell globs for folders in burrow.projectsDir that get no server: folders
        that hold a project's extra checkouts rather than a project. Linked git
        worktrees are always skipped.
      '';
    };
    spawn = lib.mkOption {
      type = lib.types.enum [ "worktree" "same-dir" ];
      default = "worktree";
      description = ''
        Where a session started from claude.ai works. worktree: its own git
        worktree of the project, so sessions working at once never share a
        checkout (an idleSession stays in the folder itself). same-dir: all
        of them in the folder. The machine's own flake
        and folders that aren't git repositories are always same-dir.
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
      default = "8G";
      example = "12G";
      description = ''
        The most memory one server may hold with all its sessions (its unit's
        MemoryMax; "infinity" for none), none of it in swap: swap would let
        it grow past the cap without ever reaching it. Past it the kernel
        kills the biggest process there, usually the session that grew, and
        the server and its other sessions carry on (OOMPolicy=continue).
        Every server is under burrow.claude.memory's cap too.
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
      default = pkgs.tmux.overrideAttrs (old: {
        configureFlags = old.configureFlags ++ [ "--disable-cgroups" ];
      });
      defaultText = lib.literalMD "tmux, built with `--disable-cgroups`";
      internal = true;
      description = ''
        The tmux the servers run in. Not tmux's usual build: that one moves
        each pane into a scope of its own, which takes a server and its
        sessions out of their unit and out from under its cap. The memory
        test starts its servers on the usual one, to see them catch up.
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
        # Under burrow.claude.memory's cap with the proj jobs (common.nix).
        Slice = "claude-rc.slice";
        # And its own, for the server and its sessions together (memoryMax).
        MemoryMax = cfg.memoryMax;
        MemorySwapMax = 0;
        OOMPolicy = "continue";
      };
    };

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
