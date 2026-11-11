# probe: read-only questions about the machine, asked on a private repo's
# issues and answered there within seconds, with no Claude
# (crates/burrow-probe). Label an issue `probe` in `repo`, as one of
# `authors`, with a question a line (`memory`, `units`, `journal UNIT SINCE
# UNTIL`, `rc`, `queue`, `deployed`, `usage [SINCE]`, `oom [SINCE]`): the
# repo's issue event starts burrow-probe.service through burrow-events, which posts the answers
# in one comment and closes the issue. Reopen it to ask again.
#
# Nothing of the issue's text reaches a command but a unit name or a time the
# probe's grammar made, journals are read only for the units on the
# allowlist, a repo GitHub doesn't call private gets no answer, and every
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
    machinery = if config.burrow.github.machineApp == null then null
      else "${config.burrow.github.machineApp.slug}[bot]";
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
    };
  };
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
      type = types.listOf types.str;
      default = config.burrow.proj.inbox.authors or [ ];
      defaultText = lib.literalExpression "config.burrow.proj.inbox.authors";
      description = ''
        The GitHub logins whose issues are answered: the inbox's, unless set.
        Anyone else's gets a line saying nothing ran, and is closed.
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
          "proj-*.service"
          "nix-daemon.service"
          "systemd-oomd.service"
        ];
        defaultText = lib.literalExpression ''[ "''${config.burrow.prefix}-*.service" "burrow-*.service" "autoland.service" "autoland@*.service" "autoland-*.service" "renovate.service" "renovate@*.service" "proj-*.service" "nix-daemon.service" "systemd-oomd.service" ]'';
        description = ''
          The system units whose journals a probe may read (globs: `*`, `?`,
          `[...]`). Keep off any whose log can hold a secret: the scrubber only
          knows secrets by their shape.
        '';
      };
      userUnits = mkOption {
        type = types.listOf types.str;
        default = [ "claude-rc@*.service" "claude-rc-*.service" "proj-*.service" "autoland-*.service" ];
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

    systemd.tmpfiles.rules = [
      "d ${config.burrow.stateDir}/probe-door 0755 ${user} ${config.users.users.${user}.group} -"
    ];

    systemd.services.burrow-probe = {
      description = "burrow-probe: answer the probe issues in ${cfg.repo}";
      # A switch leaves a running pass be (see burrow.switch's check); the
      # next one is the new system's.
      restartIfChanged = false;
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      path = [ cfg.gh ];
      environment.GH_NO_UPDATE_NOTIFIER = "1";
      environment.BURROW_DOOR_WAKE = probeWake;
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
      ${cfg.repo} labelled `${cfg.label}` (as ${lib.concatStringsSep " or " cfg.authors}), a question a line
      (`memory`, `units`, `journal UNIT SINCE UNTIL`, `rc`, `queue`, `deployed`,
      `usage [SINCE]`, `oom [SINCE]`), and the answer comes back as a comment within seconds.
      Here, `burrow-probe ask QUESTION` answers one on the terminal.
    '';
  };
}
