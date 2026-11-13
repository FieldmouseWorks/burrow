# probe: read-only questions about the machine, asked on a private repo's
# issues and answered there within seconds, with no Claude
# (crates/burrow-probe). Label an issue `probe` in `repo`, as one of
# `authors`, with a question a line (`memory`, `units`, `journal UNIT SINCE
# UNTIL`, `crashes EXE [SINCE]`, `rc`, `queue`, `deployed`, `usage [SINCE] [--totals]`, `oom [SINCE]`,
# `runners [RUNNER]`, `trust`, `issue REPO N`): the
# repo's issue event starts burrow-probe.service through burrow-events, which posts the answers
# in one comment and closes the issue. Reopen it to ask again.
#
# Nothing of the issue's text reaches a command but a unit name, executable path or time the
# probe's grammar made, journals are read only for the units on the
# allowlist, no file the reviewers never read (their skipPaths) is opened,
# listed or reached through a link, a repo GitHub doesn't call private gets no answer, and every
# answer goes through the secret scrubber before it's posted
# (tests/probe.nix plants a token to prove it). On the machine,
# `burrow-probe ask QUESTION` answers one on the terminal, unscrubbed.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.probe;
  probeWake = "${config.burrow.stateDir}/probe-door/wake.json";
  inherit (lib) mkOption types;
  inherit (config.burrow) user prefix;

  # What crates/burrow-probe reads (its Config).
  settings = {
    inherit (cfg) repo label authors;
    # Only where the updater is on: its repo reads the required origin.
    systemRepo = if config.burrow.updates.enable or false then config.burrow.updates.repo else null;
    machinery = if config.burrow.github.machineApp == null then null
      else { login = "${config.burrow.github.machineApp.slug}[bot]"; id = config.burrow.github.machineApp.accountId; };
    host = config.networking.hostName;
    inherit user;
    inherit (config.burrow) stateDir;
    journal = { inherit (cfg.journal) units userUnits; };
    proj = null;
    projects = config.burrow.projectsDir;
    jobs = if config.burrow.proj.enable or false then config.burrow.proj.jobsDir else null;
    tools = {
      git = lib.getExe config.programs.git.package;
      journalctl = "${config.systemd.package}/bin/journalctl";
      systemctl = "${config.systemd.package}/bin/systemctl";
      # `issue` reads GitHub as the machine's own calls do, never as a
      # job's login.
      gh = lib.getExe cfg.gh;
    };
    inherit runners trust hidden;
  };

  # `runners`: each runner's unit as this configuration defines it, the
  # named ones (burrow.dispatch.runners) and the template of the ones
  # started fresh for one job, whoever defines the unit. Only the
  # environment keys `runners` tells.
  dispatch = lib.attrByPath [ "burrow" "dispatch" ] { } config;
  dispatching = dispatch.enable or false;
  told = [ "PATH" "HOME" "RUSTUP_HOME" "RUSTUP_TOOLCHAIN" "CARGO_HOME" ];
  runner = kind: name: repos: labels: let
    unit = config.systemd.services.${name} or null;
    service = if unit == null then { } else unit.serviceConfig;
    environment = if unit == null then { }
      else lib.mapAttrs (_: toString) (lib.filterAttrs (k: v: lib.elem k told && v != null) unit.environment);
    user = service.User or null;
  in {
    inherit name kind repos labels environment;
    defined = unit != null;
    user = if user == null then null else toString user;
    dynamicUser = service.DynamicUser or false;
    home = environment.HOME or (if user != null && config.users.users ? ${toString user}
      then config.users.users.${toString user}.home else null);
    packages = if unit == null then [ ] else map (p: if lib.isDerivation p then p.name else toString p) unit.path;
  };
  runners = lib.optionals dispatching (
    lib.mapAttrsToList (name: r: runner "named" name [ r.repo ] r.labels) dispatch.settings.runners
    ++ lib.optional dispatch.fresh.enable (runner "fresh" dispatch.fresh.template dispatch.fresh.repos [ dispatch.fresh.label ]));

  # `trust`: the deployed trust lists and the machine's Apps. The steward's
  # authors are read from its own deployed config at answer time, so they
  # are never a second copy.
  trust = {
    inbox = if config.burrow.proj.inbox.enable or false
      then { inherit (config.burrow.proj.inbox) authors vm; } else null;
    engines = if config.burrow.proj.engines.door.enable or false
      then config.burrow.proj.engines.door.authors else null;
    apps = lib.mapAttrsToList (name: app: { inherit name; login = "${app.slug}[bot]"; id = app.accountId; }) config.burrow.github.apps;
    machinery = if config.burrow.github.machineApp == null then null else config.burrow.github.machineApp.name;
    autoland = if config.burrow.autoland.enable or false then config.burrow.autoland.configFile else null;
    sandboxes = config.burrow.agents.enable or false;
  };

  # What no answer opens, lists or resolves.
  hidden = cfg.hidden;

  probeConfig = pkgs.writeText "burrow-probe.json" (builtins.toJSON (settings // {
    proj = if config.burrow.proj.enable or false then lib.getExe' config.burrow.tools.proj "proj" else null;
  }));

  probe = pkgs.runCommand "burrow-probe" {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = "burrow-probe";
  } ''
    makeBinaryWrapper ${inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-probe}/bin/burrow-probe \
      $out/bin/burrow-probe --add-flags ${probeConfig}
  '';
in
{
  imports = [
    (import ./common.nix inputs)
    { key = "burrow/events"; imports = [ (import ./events.nix inputs) ]; }
    { key = "burrow/watchdog/sources"; imports = [ ./watchdog/sources.nix ]; }
    { key = "burrow/github"; imports = [ (import ./github.nix inputs) ]; }
  ];

  options.burrow.probe = {
    settings = mkOption { type = types.attrs; internal = true; readOnly = true; default = settings; };
    enable = lib.mkEnableOption "probes: read-only questions about the machine, asked and answered on a private repo's issues";

    repo = mkOption {
      type = types.str;
      default = "";
      example = "sam/homelab";
      description = ''
        OWNER/NAME of the private repo whose issues are probes: the machine
        flake's own, say. One GitHub doesn't call private gets no answers.
      '';
    };

    label = mkOption {
      type = types.str;
      default = "probe";
      description = "The label that makes an issue a probe.";
    };

    authors = mkOption {
      type = lib.types.listOf (import ../lib/github-author.nix { inherit lib; });
      default = config.burrow.proj.inbox.authors or [ ];
      defaultText = lib.literalExpression "config.burrow.proj.inbox.authors";
      description = ''
        The GitHub accounts, with immutable numeric IDs, whose issues are answered: the inbox's, unless set.
        Anyone else's gets a line saying nothing ran, and is closed.
      '';
    };

    hidden = mkOption {
      type = types.listOf types.str;
      default = let autoland = lib.attrByPath [ "burrow" "autoland" ] { } config; in
        lib.unique ((autoland.reviewers.skipPaths or [ ])
          ++ lib.concatLists (lib.mapAttrsToList (_: r: r.skipPaths or [ ]) (autoland.repos or { })));
      defaultText = lib.literalMD "the steward's reviewers' skipPaths, the machine's and every repo's";
      description = ''
        Path regexes (Rust's syntax) no answer opens, lists or resolves on the
        machine, held to each path, each link on the way to it and each
        link's target: what the steward's reviewers never read. One that
        doesn't compile stops every read.
      '';
    };

    journal = {
      units = mkOption {
        type = types.listOf types.str;
        default = [
          "${prefix}-*.service"
          "burrow-*.service"
          # The steward's (autoland.nix): its pass, each repo's, and the rest.
          "autoland.service"
          "autoland@*.service"
          "autoland-*.service"
          # Updates dispatches a full pass or one repository's pass.
          "renovate.service"
          "renovate@*.service"
          "renovate-sweep.service"
          "renovate-sweep@*.service"
          "proj-*.service"
          # The rest of burrow's own (checks/probe-journals.nix holds every
          # unit its modules define to this list), but gh-token's.
          "house-sync.service"
          "house-sync@*.service"
          "conaryd.service"
          "advisory-watch.service"
          "firmware-*.service"
          "agent-*.service"
          "agents-*.service"
          "prune-pre-snapshots.service"
          "zfs-delegate-*.service"
          "rust-cache-*.service"
          "hardware-watchdog-link.service"
          "nix-daemon.service"
          "systemd-oomd.service"
        ];
        defaultText = lib.literalExpression ''[ "''${config.burrow.prefix}-*.service" "burrow-*.service" "autoland.service" "autoland@*.service" "autoland-*.service" "renovate.service" "renovate@*.service" "renovate-sweep.service" "renovate-sweep@*.service" "proj-*.service" "house-sync.service" "house-sync@*.service" "conaryd.service" "advisory-watch.service" "firmware-*.service" "agent-*.service" "agents-*.service" "prune-pre-snapshots.service" "zfs-delegate-*.service" "rust-cache-*.service" "hardware-watchdog-link.service" "nix-daemon.service" "systemd-oomd.service" ]'';
        description = ''
          The system units whose journals a probe may read (globs: `*`, `?`,
          `[...]`). Keep off any whose log can hold a secret: the scrubber only
          knows secrets by their shape.
        '';
      };
      userUnits = mkOption {
        type = types.listOf types.str;
        default = [ "claude-rc@*.service" "claude-rc-*.service" "proj-*.service" "autoland-*.service" "rustup-update.service" ];
        description = "The same, for `burrow.user`'s own units (`journalctl --user-unit`).";
      };
    };

    gh = mkOption {
      type = types.package;
      default = config.burrow.github.machineGh;
      defaultText = lib.literalExpression "config.burrow.github.machineGh";
      internal = true;
      description = "The gh the probe reads and answers issues with (the test's is a stand-in).";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [{
      assertion = cfg.repo != "";
      message = "burrow.probe.repo: name the private repository for issue probes.";
    } {
      assertion = cfg.authors != [ ];
      message = "burrow.probe.authors: name who may ask the probe.";
    }];

    environment.systemPackages = [ probe ];
    burrow.tools.probe = probe;

    # Only the repo's issue events: a comment asks nothing (reopen to ask
    # again), and a pass reads every open probe anyway.
    burrow.events.handlers.probe = {
      events = [ "issues" ];
      unit = "burrow-probe";
      perRepo = false;
      repos = [ cfg.repo ];
      wake = probeWake;
      gap = 2;
    };


    # A pass GitHub's rate limit holds off writes this beside its wake file
    # (burrow-github's deferred); one past its grace is a stall.
    burrow.watchdog.sources.probe-door = {
      file = "${lib.removeSuffix ".json" probeWake}.watch.json";
      about = "The probe door";
      look = "journalctl -u burrow-probe: its pass was held off by GitHub's rate limit (`gh api rate_limit` with the workers token says until when). Questions wait meanwhile; cut the other consumers' calls on the installation.";
    };

    systemd.tmpfiles.rules = [
      "d ${config.burrow.stateDir}/probe-door 0755 ${user} ${config.users.users.${user}.group} -"
    ];

    systemd.services.burrow-probe = {
      description = "burrow-probe: answer the probe issues in ${cfg.repo}";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      path = [ cfg.gh ];
      environment.GH_NO_UPDATE_NOTIFIER = "1";
      environment.BURROW_DOOR_WAKE = probeWake;
      environment.BURROW_DOOR_WATCHDOG_KICK = lib.mkIf (config.burrow.watchdog.enable or false) "${config.burrow.events.kick}/watchdog";
      serviceConfig = {
        Type = "oneshot";
        # The user, for its user manager (`units`, `rc`) and proj (`queue`);
        # the journal's group for every unit's journal, which the allowlist
        # narrows.
        User = user;
        SupplementaryGroups = [ "systemd-journal" ];
        ExecStart = lib.getExe probe;
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "full";
        TimeoutStartSec = "5min";
      };
    };

    burrow.claude.machineMd = lib.mkOrder 702 ''
      Read-only questions about this machine need no Claude: open an issue in
      ${cfg.repo} labelled `${cfg.label}` (as ${lib.concatStringsSep " or " (map (a: a.login) cfg.authors)}), a question a line
      (`memory`, `units`, `crashes EXE [SINCE]`, `journal UNIT SINCE UNTIL`, `rc`, `queue`, `deployed`,
      `usage [SINCE] [--totals]`, `oom [SINCE]`, `runners [RUNNER]`, `trust`, `issue REPO N`), and the answer comes back as a comment within seconds.
      Here, `burrow-probe ask QUESTION` answers one on the terminal.
    '';
  };
}
