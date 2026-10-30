# usage-reset: the Claude plan's banked weekly reset gets used, once, when the
# week's usage is nearly gone, rather than every Claude on the machine
# stopping at the limit with a reset sitting unused (crates/burrow-usage-reset).
# Each pass reads the weekly meter through Claude Code itself, as burrow.user,
# whose login is the owner's plan; past the line it types /limit-reset weekly
# into a Claude Code of its own and confirms it, once a week at most, and
# HOST-notify tells the owner. With nothing banked it does nothing. It's a
# burrow-events handler that books its own next pass, sooner as the week runs
# out: no timer, and no model calls.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.usageReset;
  inherit (lib) mkOption types;
  inherit (config.burrow) user;
  name = "burrow-usage-reset";
  dir = "${config.burrow.stateDir}/usage-reset";
  package = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow;

  configFile = pkgs.writeText "${name}.json" (builtins.toJSON {
    claude = lib.getExe' config.burrow.claude.package "claude";
    tmux = lib.getExe config.burrow.tools.tmux;
    inherit (cfg) below;
    stateDir = dir;
    notify = lib.optionalString (config.burrow.tools ? notify) (lib.getExe config.burrow.tools.notify);
  });
in
{
  imports = [
    (import ./common.nix inputs)
    { key = "burrow/events"; imports = [ (import ./events.nix inputs) ]; }
  ];

  options.burrow.usageReset = {
    enable = lib.mkEnableOption ''
      the usage watcher: once the Claude plan's week has less than `below`
      percent left, it uses a banked weekly reset through Claude Code's
      /limit-reset and tells the owner. burrow.user's Claude Code has to be
      logged in to the plan
    '';

    below = mkOption {
      type = types.numbers.between 0 100;
      default = 2;
      description = ''
        The reset is used once the week has less than this percent left. A
        reset that can only be used at the limit waits for it.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    burrow.events.handlers.usage-reset = {
      # Nothing on GitHub: its own wake-ups, and the hourly catch-up.
      events = [ ];
      repos = [ ];
      perRepo = false;
      unit = name;
      wake = "${dir}/wake.json";
    };
    systemd.services.${name} = {
      description = "Use the Claude plan's banked weekly reset when the week is nearly gone";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      environment.USAGE_RESET_CONFIG = "${configFile}";
      # A switch leaves a running pass to finish, its yes perhaps pressed;
      # the next pass is the new one.
      restartIfChanged = false;
      serviceConfig = {
        Type = "oneshot";
        # Claude Code's login, and its config, are the user's.
        User = user;
        ExecStart = "${package}/bin/${name}";
        # Past the longest pass its own waits allow (a minute for each of
        # Claude Code's screens): one cut off after its yes is settled by the
        # next, never pressed again.
        TimeoutStartSec = "15min";
        # A Claude Code at a time, and a short one: the meter alone, or the
        # meter and one /limit-reset.
        MemoryMax = "2G";
        MemorySwapMax = 0;
      };
    };
    systemd.tmpfiles.settings.burrow.${dir}.d = { mode = "0755"; inherit user; group = config.users.users.${user}.group; };

    burrow.claude.machineMd = lib.mkOrder 1320 ''
      ## Usage reset

      ${name} reads the plan's weekly meter through Claude Code and, once
      the week has less than ${toString cfg.below}% left, uses a banked weekly reset with
      /limit-reset (once a week at most) and tells ${config.burrow.owner} with a notice.
      It drives its own Claude Code in `tmux -L ${name}`; leave that one
      alone. Don't run /limit-reset yourself unless ${config.burrow.owner} asks.
      ${dir}/state.json says what it last saw.
    '';
  };
}
