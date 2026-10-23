# Staying current, top to bottom, without anyone asking.
#
#   - HOST-update: every 15 minutes, bump the flake's inputs and Claude Code,
#     build, VM-test, switch with the dead man's switch, health-check,
#     confirm, push to main. Breakage reverts itself and goes to a Claude job.
#   - Renovate: dependency PRs for every repo you own, merged on green CI;
#     renovate-sweep gives the stuck ones to a Claude job.
#   - advisory-watch: new CVEs in what the box and its projects run, hourly,
#     each one handed to a Claude job the same hour.
#   - firmware-check: firmware on offer goes to the owner (flashing is theirs).
#   - rustup: the user's Rust toolchains, daily.
#
# Kernels get their own trial boot: burrow.switch.trialBoot.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.updates;
  inherit (config.burrow) prefix user stateDir tools;
  account = config.users.users.${user};
  system = pkgs.stdenv.hostPlatform.system;
  update = "${prefix}-update";
  health = "${prefix}-health";
  project = baseNameOf config.burrow.flake;
  # OWNER/NAME, when the flake lives on GitHub.
  githubRepo =
    let m = builtins.match "(https://github.com/|git@github.com:)([^/]+/[^/]+)" (lib.removeSuffix ".git" cfg.origin);
    in if m == null then null else lib.elemAt m 1;

  vars = {
    UPDATE = update;
    HEALTH = health;
    HOST = config.networking.hostName;
    PROJECT = project;
    DIR = stateDir;
    WORK = "${config.burrow.scratchDir}/${update}";
    SCRATCH = config.burrow.scratchDir;
    SRC = config.burrow.projectsDir;
    FLAKE = config.burrow.flake;
    ORIGIN = cfg.origin;
    CONFIG = config.burrow.switch.configName;
    SYSTEM = system;
    MANIFEST = if cfg.claudeManifest == null then "" else cfg.claudeManifest;
    CHECKS = lib.concatStringsSep " " cfg.checks;
    WITHIN = cfg.confirmWithin;
    CHANNEL = "nixos-${config.system.nixos.release}";
    OWNER = config.burrow.owner;
    BURROW_USER = user;
    BURROW_GROUP = account.group;
    HOME_DIR = account.home;
    USER_UID = toString account.uid;
    SWITCH = "${prefix}-switch";
    SWITCH_STATE = "/run/${prefix}-switch";
    CONFIRM = "${prefix}-confirm";
    BOOT = if config.burrow.switch.trialBoot.enable then "${prefix}-boot" else "";
    NOTIFY = "${prefix}-notify";
    REVIEW_HINTS = cfg.firmware.reviewHints;
  };

  # Each script gets the names it uses (shellcheck rejects the rest), set above
  # its own text.
  uses = text: n: lib.length (builtins.split "\\$\\{?${n}([^A-Za-z0-9_]|$)" text) > 1;
  script = name: file: runtimeInputs:
    let text = builtins.readFile file;
    in pkgs.writeShellApplication {
      inherit name runtimeInputs;
      text = lib.concatMapStrings (n: "${n}=${lib.escapeShellArg vars.${n}}\n")
        (lib.filter (uses text) (lib.attrNames vars)) + text;
    };

  calls = [ tools.proj tools.notify ];
  healthTool = script health ./updates/health.sh (with pkgs; [ coreutils gnugrep gnused gawk systemd ]);
  updateTool = script update ./updates/update.sh (
    (with pkgs; [ coreutils git jq curl util-linux systemd gnugrep gnused gawk config.nix.package ])
    ++ [ healthTool tools.switch tools.confirm ] ++ calls ++ lib.optional (tools ? boot) tools.boot
  );
  advisoryWatch = script "advisory-watch" ./updates/advisory-watch.sh (
    (with pkgs; [ coreutils curl jq gnugrep gnused gawk util-linux vulnix osv-scanner config.nix.package ]) ++ calls
  );
  firmwareCheck = script "firmware-check" ./updates/firmware-check.sh (
    (with pkgs; [ coreutils jq gnugrep gnused fwupd dmidecode nvme-cli util-linux ]) ++ calls
  );
  renovateSweep = script "renovate-sweep" ./updates/renovate-sweep.sh (
    (with pkgs; [ coreutils git gh jq gnused ]) ++ calls
  );

  # What the health checks may call, beyond the system's own PATH.
  checkPath = lib.makeBinPath ((with pkgs; [ coreutils systemd curl jq getent util-linux gnugrep ])
    ++ lib.optional config.services.tailscale.enable config.services.tailscale.package);

  # Renovate's settings for every repo (a repo's own renovate.json can add to
  # them). Checked by renovate-config-validator at build time.
  rcfg = cfg.renovate;
  renovateSettings = lib.recursiveUpdate ({
    platform = "github";
    autodiscover = true;
    autodiscoverFilter = rcfg.autodiscoverFilter;
    onboarding = false;
    requireConfig = "optional";
    dependencyDashboard = false;
    binarySource = "global";
    baseDir = "${rcfg.dir}/work";
    cacheDir = "${rcfg.dir}/cache";
    repositoryCache = "enabled";
    extends = [ "config:recommended" "helpers:pinGitHubActionDigests" ];
    prHourlyLimit = 0;
    prConcurrentLimit = 20;

    # Three days before a release counts: hijacked npm and crates releases are
    # usually caught and pulled within that. Security fixes don't wait.
    minimumReleaseAge = "3 days";
    internalChecksFilter = "strict";
    osvVulnerabilityAlerts = true;
    vulnerabilityAlerts = {
      enabled = true;
      labels = [ "security" ];
      minimumReleaseAge = null;
      automerge = true;
      schedule = [ "at any time" ];
    };

    # Transitive fixes arrive through a fresh lockfile, daily.
    lockFileMaintenance = {
      enabled = true;
      schedule = [ "before 5am" ];
      automerge = true;
    };

    # Merged by Renovate itself once every check is green, so it doesn't
    # depend on each repo's auto-merge setting.
    automergeType = "pr";
    platformAutomerge = false;

    nix.enabled = true;

    packageRules = [
      {
        matchUpdateTypes = [ "minor" "patch" "pin" "digest" "lockFileMaintenance" ];
        automerge = true;
      }
      {
        # The owner merges these; renovate-sweep has a Claude make them green first.
        matchUpdateTypes = [ "major" ];
        automerge = false;
        labels = [ "major" ];
      }
      # One PR per ecosystem per repo for everything that isn't major.
      { matchCategories = [ "rust" ]; matchUpdateTypes = [ "minor" "patch" ]; groupName = "Rust crates"; }
      { matchCategories = [ "js" ]; matchUpdateTypes = [ "minor" "patch" ]; groupName = "npm packages"; }
      { matchCategories = [ "python" ]; matchUpdateTypes = [ "minor" "patch" ]; groupName = "Python packages"; }
      { matchCategories = [ "ci" ]; matchUpdateTypes = [ "minor" "patch" "digest" "pin" ]; groupName = "CI actions"; }
      { matchCategories = [ "nix" ]; groupName = "Nix inputs"; }
    ] ++ lib.optional (githubRepo != null) {
      # The machine's own flake.lock is HOST-update's: it deploys what it bumps.
      matchRepositories = [ githubRepo ];
      matchManagers = [ "nix" ];
      enabled = false;
    };

    # rust-toolchain.toml pins follow Rust's releases too.
    customManagers = [
      {
        customType = "regex";
        managerFilePatterns = [ "/(^|/)rust-toolchain\\.toml$/" ];
        matchStrings = [ "channel\\s*=\\s*\"(?<currentValue>\\d+\\.\\d+(\\.\\d+)?)\"" ];
        depNameTemplate = "rust";
        packageNameTemplate = "rust-lang/rust";
        datasourceTemplate = "github-releases";
      }
    ];
  } // lib.optionalAttrs (rcfg.timezone != null) { inherit (rcfg) timezone; }
    // lib.optionalAttrs (rcfg.gitAuthor != null) { inherit (rcfg) gitAuthor; })
    rcfg.settings;

  renovateConfig = pkgs.runCommand "renovate-config.json"
    {
      nativeBuildInputs = [ pkgs.jq pkgs.renovate ];
      value = builtins.toJSON renovateSettings;
      passAsFile = [ "value" ];
      preferLocalBuild = true;
    }
    ''
      jq . "$valuePath" > $out
      renovate-config-validator $out
    '';

  online = {
    after = [ "network-online.target" ];
    wants = [ "network-online.target" ];
  };
in
{
  imports = [
    (import ./common.nix inputs)
    { key = "burrow/notify"; imports = [ (import ./notify.nix inputs) ]; }
    { key = "burrow/switch"; imports = [ (import ./switch.nix inputs) ]; }
    { key = "burrow/proj"; imports = [ (import ./proj.nix inputs) ]; }
  ];

  options.burrow.updates = {
    enable = lib.mkEnableOption "HOST-update and the rest of the update pipeline";

    origin = lib.mkOption {
      type = lib.types.str;
      example = "https://github.com/sam/homelab";
      description = ''
        Where the machine's flake lives (git URL). HOST-update deploys its main
        branch and pushes the bumps it tested back there, with the user's git
        login.
      '';
    };

    checks = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "rehearsal" ];
      description = "The flake's checks (VM tests) an update must pass before it goes live.";
    };

    confirmWithin = lib.mkOption {
      type = lib.types.str;
      default = "15min";
      description = "The dead man's switch window for an update (it health-checks for up to 3 minutes in it).";
    };

    every = lib.mkOption {
      type = lib.types.str;
      default = "*:0/15";
      description = "When HOST-update runs (systemd calendar event).";
    };

    claudeManifest = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "claude-code.json";
      description = ''
        burrow.claude.manifest's path inside the flake's checkout. HOST-update
        bumps it to Claude Code's newest release along with the flake inputs.
        null: Claude Code comes with nixpkgs-unstable.
      '';
    };

    health = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      default = { };
      example = { website = "curl -fsS -o /dev/null https://example.com/"; };
      description = ''
        Shell commands that exit 0 while the machine is healthy, by name.
        HOST-update compares them before and after every switch: one that passed
        before and fails after reverts the switch. burrow adds its own (sshd,
        the nix daemon, the operator, DNS, GitHub, Claude Code...).
      '';
    };

    advisories.enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "advisory-watch: new CVEs (CISA KEV, vulnix, osv-scanner) go to Claude jobs, hourly.";
    };

    firmware = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "firmware-check: what fwupd offers goes to the owner daily, plus a monthly Claude review.";
      };
      reviewHints = lib.mkOption {
        type = lib.types.str;
        default = "the board vendor's support page, the NVMe vendor's, Intel's or AMD's security advisories for the ME or PSP and microcode";
        example = "Gigabyte's page for the board, Samsung's for the 990 PRO, AMD's advisories for the PSP and microcode";
        description = "Where the monthly review should look for firmware LVFS doesn't carry.";
      };
    };

    rustup.enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "`rustup update` daily for the user, when they have toolchains in ~/.rustup.";
    };

    renovate = {
      enable = lib.mkEnableOption "a self-hosted Renovate, as the user with gh's login, for every repo it can see";
      autodiscoverFilter = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        example = [ "sam/*" ];
        description = "Which repos Renovate covers (new ones included).";
      };
      gitAuthor = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "renovate <123+sam@users.noreply.github.com>";
        description = "Who Renovate's commits are by.";
      };
      timezone = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = config.time.timeZone;
        defaultText = lib.literalExpression "config.time.timeZone";
        description = "For Renovate's schedules.";
      };
      dir = lib.mkOption {
        type = lib.types.str;
        default = "/var/cache/renovate";
        description = "Renovate's clones and caches.";
      };
      settings = lib.mkOption {
        type = lib.types.attrs;
        default = { };
        description = "Merged into burrow's Renovate settings.";
      };
      path = lib.mkOption {
        type = lib.types.listOf lib.types.package;
        default = with pkgs; [ cargo rustc nodejs_22 pnpm python3 uv go ];
        defaultText = lib.literalExpression "with pkgs; [ cargo rustc nodejs_22 pnpm python3 uv go ]";
        description = "Toolchains Renovate runs to refresh lockfiles.";
      };
    };
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [
    {
      assertions = [
        {
          assertion = (cfg.claudeManifest == null) == (config.burrow.claude.manifest == null);
          message = "Set burrow.updates.claudeManifest and burrow.claude.manifest together (the same file).";
        }
      ];

      # It deploys through the switch, hands problems to proj jobs and tells
      # the owner through notify.
      burrow.switch.enable = true;
      burrow.proj.enable = true;
      burrow.notify.enable = true;

      burrow.updates.health = lib.mapAttrs (_: lib.mkDefault) ({
        nix-daemon = "systemctl -q is-active nix-daemon.socket";
        user-manager = "systemctl -q is-active user@${vars.USER_UID}.service";
        dns = "getent hosts github.com";
        github = "curl -fsS -o /dev/null --max-time 15 https://github.com";
      } // lib.optionalAttrs config.services.openssh.enable {
        sshd = "systemctl -q is-active sshd.service";
      } // lib.optionalAttrs (config.burrow.remoteControl.enable or false) {
        operator = "systemctl --user -M ${user}@ -q is-active claude-rc@${project}.service";
      } // lib.optionalAttrs config.services.tailscale.enable {
        tailscale = "tailscale status --json | jq -e '.BackendState == \"Running\"'";
      } // lib.optionalAttrs (config.burrow.claude.enable or false) {
        # As the user, the way the operator runs it (runuser is root's only).
        claude = ''
          if [ "$(id -un)" = ${user} ]; then claude --version
          else runuser -u ${user} -- env HOME=${account.home} claude --version; fi
        '';
      });

      environment.etc = lib.mapAttrs'
        (name: cmd: lib.nameValuePair "burrow/health.d/${name}" {
          source = pkgs.writeShellScript "health-${name}" ''
            export PATH=${checkPath}:/run/wrappers/bin:/run/current-system/sw/bin
            ${cmd}
          '';
        })
        cfg.health;

      environment.systemPackages = [ updateTool healthTool ];

      # VM tests run in the Nix build sandbox, as nixbld users; let them at KVM.
      services.udev.extraRules = ''
        KERNEL=="kvm", GROUP="kvm", MODE="0666"
      '';

      systemd.services.${update} = online // {
        description = "Update the system: bump, build, test, switch, confirm";
        # Its own switch must not stop it halfway.
        restartIfChanged = false;
        # Claude Code, for the smoke test after it changes.
        path = [ "/run/current-system/sw" ];
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "${updateTool}/bin/${update}";
          TimeoutStartSec = "4h";
          Nice = 10;
        };
      };
      systemd.timers.${update} = {
        wantedBy = [ "timers.target" ];
        timerConfig = {
          OnBootSec = "10min";
          OnCalendar = cfg.every;
        };
      };

      systemd.tmpfiles.settings.burrow = {
        ${stateDir}.d = { mode = "0755"; user = "root"; group = "root"; };
        ${config.burrow.scratchDir}.d = { mode = "0755"; inherit user; group = account.group; };
      };

      burrow.claude.machineMd = lib.mkOrder 950 (''
        ## Updates

        ${config.networking.hostName} keeps itself current; nobody runs updates by hand.

        - `${update}` (timer) bumps every flake input${lib.optionalString (cfg.claudeManifest != null) " and Claude Code"},
          builds, runs the VM tests, switches, checks health, confirms and pushes
          to main. It deploys main too, so a merged PR goes live the same way. It
          holds off while a switch is pending or while ${config.networking.hostName} runs changes that
          aren't on main, so commit and push what you switch to.
        - `${update} status` says what the last run did. When ${config.burrow.owner} asks how
          updates are going, start there${lib.optionalString (vars.BOOT != "") ", plus `${vars.BOOT} status`"}.
      '' + lib.optionalString rcfg.enable ''
        - Renovate opens and merges dependency PRs in every repo; renovate-sweep
          hands the stuck ones to Claude jobs.
      '' + lib.optionalString cfg.advisories.enable ''
        - advisory-watch hands new CVEs to Claude jobs. Jobs from it show up in
          `proj status`.
      '' + ''
        - Things ${config.burrow.owner} must know go to ${if config.burrow.notify.repo == null then "the journal" else "issues labelled `${config.burrow.notify.label}` in ${config.burrow.notify.repo}"}
          (`${vars.NOTIFY}`).${lib.optionalString cfg.firmware.enable " Flashing firmware stays their call."}
      '');
    }

    (lib.mkIf cfg.advisories.enable {
      environment.systemPackages = [ advisoryWatch pkgs.vulnix pkgs.osv-scanner ];
      systemd.services.advisory-watch = online // {
        description = "Look for new vulnerabilities in what the system and its projects run";
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "${advisoryWatch}/bin/advisory-watch";
          TimeoutStartSec = "1h";
          Nice = 10;
        };
      };
      systemd.timers.advisory-watch = {
        wantedBy = [ "timers.target" ];
        timerConfig = {
          OnBootSec = "20min";
          OnCalendar = "hourly";
          RandomizedDelaySec = "10min";
        };
      };
    })

    (lib.mkIf cfg.firmware.enable {
      environment.systemPackages = [ firmwareCheck ];
      services.fwupd.enable = true;
      systemd.services.firmware-check = online // {
        description = "Look for firmware updates (installing them is the owner's call)";
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "${firmwareCheck}/bin/firmware-check";
        };
      };
      systemd.timers.firmware-check = {
        wantedBy = [ "timers.target" ];
        timerConfig = {
          OnCalendar = "daily";
          Persistent = true;
          RandomizedDelaySec = "1h";
        };
      };
      systemd.services.firmware-review = {
        description = "Monthly look at vendor firmware that LVFS doesn't carry";
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "${firmwareCheck}/bin/firmware-check --review";
        };
      };
      systemd.timers.firmware-review = {
        wantedBy = [ "timers.target" ];
        timerConfig = {
          OnCalendar = "monthly";
          Persistent = true;
        };
      };
    })

    (lib.mkIf rcfg.enable {
      environment.systemPackages = [ renovateSweep pkgs.renovate ];

      # Renovate as the user, with gh's login, like the other jobs that talk to GitHub.
      systemd.services.renovate = online // {
        description = "Renovate: dependency PRs for every repo";
        environment = {
          RENOVATE_CONFIG_FILE = renovateConfig;
          LOG_LEVEL = "info";
        };
        path = [ pkgs.renovate pkgs.gh pkgs.git pkgs.openssh config.nix.package ] ++ rcfg.path;
        serviceConfig = {
          Type = "oneshot";
          User = user;
          TimeoutStartSec = "2h";
          Nice = 10;
        };
        script = ''
          RENOVATE_TOKEN=$(gh auth token)
          export RENOVATE_TOKEN GITHUB_COM_TOKEN=$RENOVATE_TOKEN
          exec renovate
        '';
      };
      systemd.timers.renovate = {
        wantedBy = [ "timers.target" ];
        timerConfig = {
          OnBootSec = "30min";
          OnCalendar = "hourly";
        };
      };

      systemd.services.renovate-sweep = {
        description = "Hand stuck Renovate PRs to a Claude job";
        after = [ "renovate.service" ];
        serviceConfig = {
          Type = "oneshot";
          User = user;
          ExecStart = "${renovateSweep}/bin/renovate-sweep";
        };
      };
      systemd.timers.renovate-sweep = {
        wantedBy = [ "timers.target" ];
        timerConfig.OnCalendar = "*-*-* 0/4:40:00";
      };

      systemd.tmpfiles.settings.burrow = {
        ${rcfg.dir}.d = { mode = "0755"; inherit user; group = account.group; };
        "${stateDir}/renovate-sweep".d = { mode = "0755"; inherit user; group = account.group; };
      };
    })

    (lib.mkIf cfg.rustup.enable {
      # Rust toolchains live in ~/.rustup, outside Nix; keep them current.
      systemd.user.services.rustup-update = {
        description = "rustup update";
        unitConfig = {
          ConditionUser = user;
          ConditionPathIsDirectory = "%h/.rustup/toolchains";
        };
        path = [ pkgs.rustup ];
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "${pkgs.rustup}/bin/rustup update";
          Nice = 10;
        };
      };
      systemd.user.timers.rustup-update = {
        wantedBy = [ "timers.target" ];
        unitConfig.ConditionUser = user;
        timerConfig = {
          OnCalendar = "daily";
          Persistent = true;
          RandomizedDelaySec = "1h";
        };
      };
    })
  ]);
}
