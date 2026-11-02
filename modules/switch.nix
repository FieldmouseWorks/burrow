# A dead man's switch for system changes: HOST-switch puts a new system live
# without making it the boot default, and unless HOST-confirm runs in time the
# old one comes back by itself. So an agent can change anything about the box,
# including what it needs to reach it, and a mistake undoes itself.
#
# With trialBoot, the same goes for kernels: HOST-boot boots a new kernel,
# initrd or kernel command line once, at a quiet hour, and GRUB only defaults
# to it after it came up.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.switch;
  inherit (config.burrow) prefix user stateDir;
  switch = "${prefix}-switch";
  confirm = "${prefix}-confirm";
  boot = "${prefix}-boot";

  vars = {
    SWITCH = switch;
    CONFIRM = confirm;
    BOOT = if cfg.trialBoot.enable then boot else "";
    REVERT = "${prefix}-revert";
    STATE = "/run/${prefix}-switch";
    FLAKE = config.burrow.flake;
    CONFIG = cfg.configName;
    WITHIN = cfg.confirmWithin;
  };

  # Each script gets the names it uses, set above its own text.
  script = name: file: names: runtimeInputs: pkgs.writeShellApplication {
    inherit name runtimeInputs;
    text = lib.concatMapStrings (n: "${n}=${lib.escapeShellArg vars.${n}}\n") names
      + builtins.readFile file;
  };
  base = [ pkgs.coreutils pkgs.util-linux pkgs.systemd ];

  # HOST-boot (crates/burrow-boot, whose lib.rs says what each of these
  # is), with its config built in.
  bootConfig = pkgs.writeText "boot.json" (builtins.toJSON {
    inherit boot user;
    host = config.networking.hostName;
    state = stateDir;
    switchState = vars.STATE;
    booted = "/run/booted-system";
    current = "/run/current-system";
    grubCfg = "/boot/grub/grub.cfg";
    grubDirs = [ "/boot" "/boot-fallback" ];
    tools = {
      sudo = "${config.security.wrapperDir}/sudo";
      grubEditenv = "${pkgs.grub2}/bin/grub-editenv";
      systemctl = "${config.systemd.package}/bin/systemctl";
      notify = lib.getExe config.burrow.tools.notify;
      logger = "${config.systemd.package}/bin/systemd-cat";
    };
  });
  bootTool = pkgs.runCommand boot {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = boot;
  } ''
    makeBinaryWrapper ${inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-boot}/bin/burrow-boot $out/bin/${boot} \
      --add-flags ${bootConfig}
  '';
  switchTool = script switch ./switch/switch.sh [ "SWITCH" "CONFIRM" "REVERT" "STATE" "FLAKE" "CONFIG" "WITHIN" ]
    (base ++ [ pkgs.jq config.nix.package ]);
  confirmTool = script confirm ./switch/confirm.sh [ "REVERT" "STATE" "BOOT" ]
    (base ++ [ config.nix.package ] ++ lib.optional cfg.trialBoot.enable bootTool);

  watchdog = config.systemd.settings.Manager.RuntimeWatchdogSec or null != null;

  # A time span in seconds: one number and unit. Zero, infinity and anything
  # else count as unbounded (null), so the check below errs towards flagging.
  seconds = span:
    let
      m = builtins.match "([0-9]+) *([a-z]*)" (toString span);
      unit = {
        "" = 1; s = 1; sec = 1; second = 1; seconds = 1;
        m = 60; min = 60; minute = 60; minutes = 60;
        h = 3600; hr = 3600; hour = 3600; hours = 3600;
        d = 86400; day = 86400; days = 86400;
      }.${lib.elemAt m 1} or null;
    in if m == null || unit == null || lib.toInt (lib.head m) == 0 then null
      else lib.toInt (lib.head m) * unit;

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
        description = "When a queued trial boot happens (systemd calendar event). Pick a quiet hour.";
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
      }];

      environment.systemPackages = [ switchTool confirmTool ];
      burrow.tools = { switch = switchTool; confirm = confirmTool; };

      # The commit the system was built from, `-dirty` from a dirty tree:
      # `nixos-version --configuration-revision` says it, and HOST-update and
      # the pipeline dispatcher read it to tell what's running.
      system.configurationRevision = lib.mkIf (cfg.flakeInputs ? self)
        (lib.mkDefault (cfg.flakeInputs.self.rev or cfg.flakeInputs.self.dirtyRev or null));

      environment.etc."burrow/inputs.json" = lib.mkIf (cfg.flakeInputs != { }) {
        text = builtins.toJSON (
          lib.mapAttrs (_: i: { rev = i.rev or null; lastModified = i.lastModified or null; })
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
        restartIfChanged = false;
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "${bootTool}/bin/${boot} trial";
        };
      };
      systemd.timers."${boot}-trial" = {
        wantedBy = [ "timers.target" ];
        timerConfig.OnCalendar = cfg.trialBoot.at;
      };
    })
  ]);
}
