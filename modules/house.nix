# house: every repo's copy of the house block (house/rules.md, between its
# markers in AGENTS.md) kept the same as burrow's (house/sync.sh). A change to
# the rules here reaches each repo as one draft PR, which the steward lands.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.house;
  inherit (config.burrow) user;
  inherit (lib) mkOption types;

  sync = pkgs.writeShellApplication {
    name = "house-sync";
    runtimeInputs = with pkgs; [ gh jq coreutils gnugrep gnused diffutils ];
    text = ''
      RULES=${../house/rules.md}
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
      default = config.burrow.autoland.exclude or [ ];
      defaultText = lib.literalExpression "config.burrow.autoland.exclude";
      description = "Repos to leave alone (OWNER/NAME globs).";
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

    systemd.services.house-sync = {
      description = "house-sync: every repo's house block, as burrow has it";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      serviceConfig = {
        Type = "oneshot";
        User = user;
        ExecStart = "${sync}/bin/house-sync";
        TimeoutStartSec = "20min";
        Nice = 10;
      };
    };
    systemd.timers.house-sync = {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnCalendar = "daily";
        RandomizedDelaySec = "1h";
        Persistent = true;
      };
    };

    burrow.claude.machineMd = lib.mkOrder 1060 ''
      ## The house rules

      The house rules block in every repo's AGENTS.md is a copy of burrow's
      house/rules.md, and `house-sync` (daily) opens a PR wherever a copy differs.
      To change a house rule, change it in burrow, never between a repo's markers.
    '';
  };
}
