# A dead man's switch for system changes: HOST-switch puts a new system live
# without making it the boot default, and unless HOST-confirm runs in time the
# old one comes back by itself. So an agent can change anything about the box,
# including what it needs to reach it, and a mistake undoes itself.
#
# With trialBoot, the same goes for kernels: HOST-boot boots a new kernel,
# initrd or kernel command line once, when idle, and GRUB only defaults
# to it after it came up.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.switch;
  inherit (config.burrow) prefix user stateDir;
  switch = "${prefix}-switch";
  confirm = "${prefix}-confirm";
  boot = "${prefix}-boot";

  switchState = "/run/${prefix}-switch";
  system = pkgs.stdenv.hostPlatform.system;
  # A binary of one of burrow's crates, with its config built in.
  wrapped = name: crate: bin: conf: pkgs.runCommand name {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = name;
  } ''
    makeBinaryWrapper ${inputs.self.packages.${system}.${crate}}/bin/${bin} $out/bin/${name} \
      --add-flags ${conf}
  '';

  # HOST-boot (crates/burrow-boot, whose lib.rs says what each of these
  # is).
  bootTool = wrapped boot "burrow-boot" "burrow-boot" (pkgs.writeText "boot.json" (builtins.toJSON {
    inherit boot user switchState;
    busyUnits = [ "${prefix}-update.service" "autoland*.service" ];
    runners = lib.mapAttrsToList (name: _: "github-runner-${name}.service") (config.services.github-runners or { })
      ++ lib.mapAttrsToList (name: _: "${name}.service") (config.burrow.dispatch.runners or { })
      ++ lib.optional (config.burrow.dispatch.fresh.enable or false) "${config.burrow.dispatch.fresh.template}*.service";
    host = config.networking.hostName;
    state = stateDir;
    booted = "/run/booted-system";
    current = "/run/current-system";
    grubCfg = "/boot/grub/grub.cfg";
    grubDirs = [ "/boot" "/boot-fallback" ];
    tools = {
      sudo = "${config.security.wrapperDir}/sudo";
      grubEditenv = "${pkgs.grub2}/bin/grub-editenv";
      systemctl = "${config.systemd.package}/bin/systemctl";
      journalctl = "${config.systemd.package}/bin/journalctl";
      notify = lib.getExe config.burrow.tools.notify;
      logger = "${config.systemd.package}/bin/systemd-cat";
    };
  }));

  # HOST-switch and HOST-confirm (crates/burrow-switch, whose lib.rs says
  # what each of these is), one config for both.
  switchConfig = pkgs.writeText "switch.json" (builtins.toJSON {
    inherit switch confirm;
    revert = "${prefix}-revert";
    state = switchState;
    flake = config.burrow.flake;
    configuration = cfg.configName;
    within = cfg.confirmWithin;
    current = "/run/current-system";
    profile = "/nix/var/nix/profiles/system";
    activationLock = "/run/nixos/switch-to-configuration.lock";
    path = lib.makeBinPath [ pkgs.coreutils pkgs.util-linux config.systemd.package ];
    tools = {
      sudo = "${config.security.wrapperDir}/sudo";
      systemctl = "${config.systemd.package}/bin/systemctl";
      systemdRun = "${config.systemd.package}/bin/systemd-run";
      systemdAnalyze = "${config.systemd.package}/bin/systemd-analyze";
      logger = "${config.systemd.package}/bin/systemd-cat";
      runuser = "${pkgs.util-linux}/bin/runuser";
      nix = "${config.nix.package}/bin/nix";
      nixEnv = "${config.nix.package}/bin/nix-env";
      boot = if cfg.trialBoot.enable then lib.getExe bootTool else null;
    };
  });
  switchTool = wrapped switch "burrow-switch" "burrow-switch" switchConfig;
  confirmTool = wrapped confirm "burrow-switch" "burrow-confirm" switchConfig;

  watchdog = config.systemd.settings.Manager.RuntimeWatchdogSec or null != null;

  # A time span in seconds: one number and unit. Zero, infinity and anything
  # else count as unbounded (null), so the check below errs towards flagging.
  seconds = span:
    let
      m = if builtins.isString span || builtins.isInt span
        then builtins.match "([0-9]+) *([a-z]*)" (toString span) else null;
      unit = if m == null then null else {
        "" = 1; s = 1; sec = 1; second = 1; seconds = 1;
        m = 60; min = 60; minute = 60; minutes = 60;
        h = 3600; hr = 3600; hour = 3600; hours = 3600;
        d = 86400; day = 86400; days = 86400;
      }.${lib.elemAt m 1} or null;
      # Strip leading zeros before conversion; reject huge integers before
      # lib.toInt can overflow. systemd stores microseconds in uint64_t, with
      # UINT64_MAX reserved for infinity. parse_time rejects equality with
      # floor(UINT64_MAX / microseconds-per-unit), too. Divide before
      # multiplying in Nix, preserving that strict bound for every unit.
      digits = if m == null then null else builtins.match "0*([1-9][0-9]*)" (lib.head m);
      n = if digits == null || builtins.stringLength (lib.head digits) > 14
        then null else lib.toInt (lib.head digits);
    in if unit == null || n == null || n >= builtins.div 18446744073709 unit then null
      else n * unit;

  # Stops can belong to an earlier activation or to the revert itself. There
  # is no safe way to attribute PID 1's jobs to a killed activation, so require
  # bounded stops on every enabled service, even ones not restarted on change.
  # TimeoutSec supplies both timeouts unless the stop timeout overrides it.
  defaultStop = config.systemd.settings.Manager.DefaultTimeoutStopSec or "90s";
  stopTimeout = sc: sc.TimeoutStopSec or (sc.TimeoutSec or defaultStop);
  defaultAbort = config.systemd.settings.Manager.DefaultTimeoutAbortSec or "";
  abortTimeout = sc:
    let t = sc.TimeoutAbortSec or defaultAbort;
    in if t == "" then stopTimeout sc else t;
  unboundedStops = lib.attrNames (lib.filterAttrs (_: s:
    s.enable && (seconds (stopTimeout s.serviceConfig) == null
      || (let sc = s.serviceConfig; in
        !builtins.elem (sc.TimeoutStopFailureMode or "terminate") [ "terminate" "abort" "kill" ]
        || !builtins.elem (sc.TimeoutStartFailureMode or "terminate") [ "terminate" "abort" "kill" ]
        ||
        ((sc.TimeoutStopFailureMode or "terminate") == "abort"
          || (sc.TimeoutStartFailureMode or "terminate") == "abort")
        && seconds (abortTimeout sc) == null))
  ) config.systemd.services);

  # RemainAfterExit as systemd reads it: free-form, so a Nix bool or any of
  # its words. Unset, or one it wouldn't read as true, is false.
  remains = sc: builtins.elem (lib.toLower (toString (sc.RemainAfterExit or false))) [ "1" "yes" "true" "on" "y" "t" ];

  # A switch restarts a changed unit that is running (or, with
  # RemainAfterExit, has run) and waits for it. A oneshot that may run longer
  # than the confirm window then outlasts the activation's deadline, so the
  # switch gets killed and reverted for a job that was only slow. A pass of
  # more than a minute that exits when it's done (no RemainAfterExit) is
  # likely to be caught mid-run whatever its timeout: on a slow box it hits
  # that timeout inside the switch, which fails the activation and reverts it
  # just the same. A minute or less is a reload, such as NixOS's nginx's.
  # Only a declared timeout counts: most of NixOS's own oneshots set none and
  # are meant to rerun on a switch, so a job that can run long says so.
  longOneshots = lib.attrNames (lib.filterAttrs (_: s:
    let
      sc = s.serviceConfig;
      t = if sc.TimeoutStartSec or null != null then sc.TimeoutStartSec else sc.TimeoutSec or null;
    in s.enable && s.restartIfChanged
      && (sc.Type or null) == "oneshot"
      && t != null
      && (let n = seconds t; w = seconds cfg.confirmWithin; in n == null || w == null || n >= w
        || (n > 60 && !remains sc))
  ) config.systemd.services);
in
{
  imports = [
    (import ./common.nix inputs)
    { key = "burrow/notify"; imports = [ (import ./notify.nix inputs) ]; }
  ];

  options.burrow.switch = {
    enable = lib.mkEnableOption "HOST-switch and HOST-confirm, system changes with a dead man's switch";

    configName = lib.mkOption {
      type = lib.types.str;
      default = config.networking.hostName;
      defaultText = lib.literalExpression "config.networking.hostName";
      description = "Which nixosConfigurations entry of burrow.flake to build.";
    };

    confirmWithin = lib.mkOption {
      type = lib.types.str;
      default = "10min";
      description = "How long a switch waits for HOST-confirm before it reverts (systemd time span).";
    };

    flakeInputs = lib.mkOption {
      type = lib.types.lazyAttrsOf lib.types.raw;
      default = { };
      example = lib.literalExpression "inputs";
      description = ''
        The machine flake's inputs. Their revisions go in /etc/burrow/inputs.json,
        self's is the system's configuration revision (unless the flake sets
        its own), and HOST-switch refuses a system built from an older nixpkgs
        than the running one (it would quietly undo security fixes) unless told
        --allow-downgrade.
      '';
    };

    trialBoot = {
      enable = lib.mkEnableOption ''
        trial boots (GRUB only): a new kernel, initrd or kernel command line
        boots once before it becomes the default. Also sets panic=10, and has a
        systemd initrd that fails reboot, so a bad kernel resets into the last
        good one by itself'';

      at = lib.mkOption {
        type = lib.types.str;
        default = "*-*-* 05:15:00";
        description = "When a queued trial boot is tried each day, and the only time one queued two days or more goes whatever work is running. Idle completion events try earlier, and never over running work.";
      };
    };
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [
    {
      assertions = [{
        assertion = longOneshots == [ ];
        message = ''
          These oneshot services may run longer than burrow.switch.confirmWithin
          (${cfg.confirmWithin}), or are passes a switch can catch mid-run, and a switch that
          finds one running restarts it and waits, reverting if it times out:
          ${lib.concatStringsSep ", " longOneshots}. Give each restartIfChanged = false,
          so a switch leaves a running one be and the next run is the new one.'';
      } {
        assertion = seconds defaultStop != null && unboundedStops == [ ];
        message = ''
          burrow.switch needs bounded service stops so the revert cannot wait
          forever behind ExecStop, ExecStopPost, or a SIGTERM-deaf start.
          Set systemd.settings.Manager.DefaultTimeoutStopSec and each enabled
          service's effective TimeoutStopSec (including TimeoutSec) to a positive
          integer with a seconds, minutes, hours or days unit, never 0 or infinity.
          Values must be strings or integers within systemd's microsecond range.
          Abort failure modes also need a bounded TimeoutAbortSec (inheriting
          DefaultTimeoutAbortSec, or falling back to TimeoutStopSec when empty).
          Unbounded or unrecognised service timeouts: ${lib.concatStringsSep ", " unboundedStops}.
          restartIfChanged = false does not exempt a service: dependencies and
          removing a unit can still stop it during a switch or revert.'';
      }];

      environment.systemPackages = [ switchTool confirmTool ];
      burrow.tools = { switch = switchTool; confirm = confirmTool; };

      # switch-to-configuration waits on every job it queues, so a unit it
      # restarts that is ordered after a long oneshot already running (a
      # sweep after a two-hour renovate) would hold the activation past its
      # deadline, and the switch would revert with nothing wrong in it
      # (burrow#811). Patched to stop waiting on jobs that wait on nothing
      # but runs going since before the activation: they stay queued as they
      # are and run once the run ends (held.rs says how it tells). The setup
      # sysinit-reactivation.target reruns is still waited for whole: the
      # rest of the system counts on it. The revert's activation is a system
      # built with it too. The patch stops applying, and the build says so,
      # if nixpkgs' main.rs moves under it.
      nixpkgs.overlays = [
        (_: prev: {
          switch-to-configuration-ng = prev.switch-to-configuration-ng.overrideAttrs (old: {
            patches = (old.patches or [ ]) ++ [ ../patches/switch-to-configuration-ng/main.patch ];
            postPatch = (old.postPatch or "") + ''
              cp ${../patches/switch-to-configuration-ng/held.rs} src/held.rs
              mkdir -p src/held
              cp ${../patches/switch-to-configuration-ng/held/tests.rs} src/held/tests.rs
            '';
          });
        })
      ];

      # The commit the system was built from, `-dirty` from a dirty tree:
      # `nixos-version --configuration-revision` says it, and HOST-update and
      # the pipeline dispatcher read it to tell what's running.
      system.configurationRevision = lib.mkIf (cfg.flakeInputs ? self)
        (lib.mkDefault (cfg.flakeInputs.self.rev or cfg.flakeInputs.self.dirtyRev or null));

      environment.etc."burrow/inputs.json" = lib.mkIf (cfg.flakeInputs != { }) {
        text = builtins.toJSON (
          lib.mapAttrs (name: i: {
            rev = i.rev or null;
            lastModified = i.lastModified or null;
            repo = let
              lockPath = if cfg.flakeInputs ? self then cfg.flakeInputs.self.outPath + "/flake.lock" else null;
              lock = if lockPath != null && builtins.pathExists lockPath then builtins.fromJSON (builtins.readFile lockPath) else { };
              node = lock.nodes.${lock.root or "root"}.inputs.${name} or null;
              source = if builtins.isString node then lock.nodes.${node}.locked or { } else { };
            in (import ../lib/locked-repo.nix { inherit lib; }) source;
          })
            (removeAttrs cfg.flakeInputs [ "self" ])
        );
      };

      burrow.claude.machineMd = lib.mkOrder 900 (''
        ## Changing the system

        1. Edit the flake in ${config.burrow.flake}.
        2. `${switch}` builds it and puts it live, but doesn't make it the boot
           default. Unless `${confirm}` runs within ${cfg.confirmWithin}, the previous system
           comes back by itself. If the change cuts you off (network, SSH, Tailscale,
           this session), just wait and it reverts. Run it detached,
           `sudo systemd-run --unit=${switch}-run --collect ${switch}`, and follow
           `journalctl -u ${switch}-run` in short looks: a switch that restarts your
           own session would otherwise die half done.
        3. Check that what you changed works, then run `${confirm}`.
        4. ${if lib.attrByPath [ "burrow" "updates" "throughTrain" ] false config
          then "Commit, push a branch and open a PR against main: only the merge train moves main, once the steward has settled the PR."
          else "Commit and push to main."} main must always match what ${config.networking.hostName} runs.

        `${switch} --status` shows anything pending, and `${switch} --abort` reverts
        at once instead of waiting. `nixos-rebuild switch --rollback` or the boot menu
        goes further back.${lib.optionalString watchdog " A hung box gets reset by the hardware watchdog and\nboots the last confirmed generation."}
      '' + lib.optionalString (cfg.flakeInputs != { }) ''

        ${switch} refuses a system built from an older nixpkgs than the running
        one; pull origin/main first, or pass `--allow-downgrade` when you mean it.
      '' + lib.optionalString cfg.trialBoot.enable ''

        A new kernel, initrd or kernel command line boots once at the next trial
        (or `sudo ${boot} trial` now) and only becomes the boot default after it
        comes up. A kernel that fails is never tried again. `${boot} status` shows
        what's queued.
      '');
    }

    (lib.mkIf cfg.trialBoot.enable {
      assertions = [{
        assertion = config.boot.loader.grub.enable;
        message = "burrow.switch.trialBoot needs GRUB (it boots a trial through grubenv's next_entry).";
      }];

      environment.systemPackages = [ bootTool ];
      burrow.tools.boot = bootTool;

      # Panic means reboot (GRUB then boots the last good generation), and so
      # does an initrd that can't mount the system: there may be no console.
      boot.kernelParams = [ "panic=10" ];
      boot.initrd.systemd.targets.initrd.unitConfig.OnFailure =
        lib.mkIf config.boot.initrd.systemd.enable [ "" "reboot.target" ];

      systemd.tmpfiles.settings.burrow.${stateDir}.d = { mode = "0755"; user = "root"; group = "root"; };

      # Settle a trial boot and remember which kernels have booted here.
      systemd.services."${boot}-check" = {
        description = "Settle a trial boot (${boot})";
        wantedBy = [ "multi-user.target" ];
        after = [ "multi-user.target" "sshd.service" ];
        restartIfChanged = false;
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "${bootTool}/bin/${boot} check";
        };
      };

      systemd.services."${boot}-trial" = {
        description = "Boot a new kernel once, if one is waiting";
        after = [ "${boot}-check.service" ];
        restartIfChanged = false;
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "${bootTool}/bin/${boot} trial";
        };
      };
      systemd.services."${boot}-trial-watch" = {
        description = "Retry queued trial boots on work completion";
        # A target normally orders itself after its wanted services. The
        # settlement check runs after multi-user.target, so opt out of that
        # implicit ordering as well as waiting for settlement.
        after = [ "multi-user.target" "${boot}-check.service" ];
        wantedBy = [ "multi-user.target" ];
        # Unlike the work it observes, the listener can restart safely. Its
        # wrapped command captures runner patterns and must follow switches.
        restartIfChanged = true;
        serviceConfig = {
          ExecStart = "${bootTool}/bin/${boot} watch";
          Restart = "always";
          RestartSec = 5;
        };
      };
      # The daily timer's own start: the only one that may go over work
      # running, once the trial has waited two days.
      systemd.services."${boot}-trial-deadline" = {
        description = "Boot a new kernel once, if one is waiting, over work running two days on";
        after = [ "${boot}-check.service" ];
        restartIfChanged = false;
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "${bootTool}/bin/${boot} trial --deadline";
        };
      };
      systemd.paths."${boot}-trial" = {
        wantedBy = [ "multi-user.target" ];
        pathConfig = {
          PathChanged = [ "${stateDir}/boot-trial" switchState ];
          Unit = "${boot}-trial.service";
        };
      };
      systemd.timers."${boot}-trial" = {
        wantedBy = [ "timers.target" ];
        timerConfig = {
          OnCalendar = cfg.trialBoot.at;
          Unit = "${boot}-trial-deadline.service";
        };
      };
    })
  ]);
}
