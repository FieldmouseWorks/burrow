# house: every repo's copy of the house block (house/rules.md, between its
# markers in AGENTS.md) kept the same as burrow's (house/sync.sh). A change to
# the rules here reaches each repo as one PR, which the steward lands. Nothing
# waits for a timer: a switch that brings new rules starts a pass over every
# repo, and a push to a repo's default branch one over that repo.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.house;
  inherit (config.burrow) user;
  inherit (lib) mkOption types;
  # The rules the running system has: /etc moves to a switch's new ones
  # before it starts anything, so a pass started under older rules can tell.
  current = "burrow/house-rules.md";
  lockDir = "/run/house-sync";

  sync = pkgs.writeShellApplication {
    name = "house-sync";
    runtimeInputs = with pkgs; [ gh jq coreutils gnugrep gnused diffutils util-linux ];
    text = ''
      RULES=${../house/rules.md}
      CURRENT=/etc/${current}
      LOCK=${lockDir}/lock
      OWNERS=${lib.escapeShellArg (lib.concatStringsSep " " cfg.owners)}
      EXCLUDE=${lib.escapeShellArg (lib.concatStringsSep " " cfg.exclude)}
      NOTIFY=${config.burrow.tools.notify}/bin/${config.burrow.prefix}-notify
      ${lib.optionalString cfg.dryRun "DRY_RUN=1"}
    '' + builtins.readFile ./house/sync.sh;
  };
in
{
  imports = [
    (import ./common.nix inputs)
    { key = "burrow/notify"; imports = [ (import ./notify.nix inputs) ]; }
    { key = "burrow/events"; imports = [ (import ./events.nix inputs) ]; }
  ];

  options.burrow.house = {
    enable = lib.mkEnableOption "house-sync, every repo's house block kept current";

    owners = mkOption {
      type = types.listOf types.str;
      default = config.burrow.autoland.owners or [ ];
      defaultText = lib.literalExpression "config.burrow.autoland.owners";
      description = "GitHub users and orgs whose repos it keeps (every non-archived, non-fork repo).";
    };

    exclude = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = ''
        Repos to leave alone (OWNER/NAME globs). Not autoland's exclude: a repo
        that lands its own PRs still keeps the house rules, and gets its
        house PR to land its own way.
      '';
    };

    dryRun = mkOption {
      type = types.bool;
      default = false;
      description = "Only log what a pass would do (`house-sync status`), changing nothing on GitHub.";
    };
  };

  config = lib.mkIf cfg.enable {
    burrow.notify.enable = true;
    environment.systemPackages = [ sync ];
    environment.etc.${current}.source = ../house/rules.md;
    # Every pass, full or one repo's, takes this lock: burrow-events lines
    # them up, but not across its own restarts, and two passes at once can
    # each close the other's PR.
    systemd.tmpfiles.rules = [ "d ${lockDir} 0755 ${user} ${config.users.users.${user}.group} -" ];

    # A push to a repo's default branch (a house PR merged, AGENTS.md edited
    # by hand, a new repo's first commits) starts house-sync@OWNER-REPO, its
    # pass over that repo. burrow-events' catch-up, when it starts and hourly,
    # is the full pass, for what no push reports: a house PR closed unmerged.
    burrow.events.handlers.house = {
      events = [ "push" ];
      unit = "house-sync";
      pushes = "default";
    };
    systemd.services = lib.genAttrs [ "house-sync" "house-sync@" ] (name: {
      description = "house-sync: ${if name == "house-sync" then "every repo's" else "%I's"} house block, as burrow has it";
      # A switch leaves a running one be (see burrow.switch's check).
      restartIfChanged = false;
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      serviceConfig = {
        Type = "oneshot";
        User = user;
        ExecStart = "${sync}/bin/house-sync${lib.optionalString (name == "house-sync@") " %I"}";
        TimeoutStartSec = "20min";
        Nice = 10;
      };
    }) // {
      # New rules arrive with a switch, which changes this unit and so starts
      # it again: it asks burrow-events for a full pass, in line with the
      # handler's others.
      house-sync-rules = {
        description = "house-sync: a full pass for the house rules this system has";
        wantedBy = [ "multi-user.target" ];
        after = [ "burrow-events.service" ];
        restartTriggers = [ ../house/rules.md ];
        path = [ pkgs.curl ];
        script = "curl -fsS --retry 10 --retry-connrefused --retry-delay 1 -d '' ${config.burrow.events.kick}/house";
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
        };
      };
    };

    burrow.claude.machineMd = lib.mkOrder 1060 ''
      ## The house rules

      The house rules block in every repo's AGENTS.md is a copy of burrow's
      house/rules.md, and `house-sync` (on new rules, and each repo's pushes to
      its default branch) opens a PR wherever a copy differs.
      To change a house rule, change it in burrow, never between a repo's markers.
    '';
  };
}
