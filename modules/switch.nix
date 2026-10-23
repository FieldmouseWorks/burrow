# A dead man's switch for system changes: HOST-switch puts a new system live
# without making it the boot default, and unless HOST-confirm runs in time the
# old one comes back by itself. So an agent can change anything about the box,
# including what it needs to reach it, and a mistake undoes itself.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.switch;
  switch = "${cfg.prefix}-switch";
  confirm = "${cfg.prefix}-confirm";

  vars = {
    SWITCH = switch;
    CONFIRM = confirm;
    REVERT = "${cfg.prefix}-revert";
    STATE = "/run/${cfg.prefix}-switch";
    FLAKE = config.burrow.flake;
    CONFIG = cfg.configName;
    WITHIN = cfg.confirmWithin;
  };

  # Each script gets the names it uses, set above its own text.
  script = name: file: names: pkgs.writeShellApplication {
    inherit name;
    runtimeInputs = [ pkgs.coreutils pkgs.util-linux pkgs.systemd config.nix.package ];
    text = lib.concatMapStrings (n: "${n}=${lib.escapeShellArg vars.${n}}\n") names
      + builtins.readFile file;
  };
in
{
  imports = [ (import ./common.nix inputs) ];

  options.burrow.switch = {
    enable = lib.mkEnableOption "HOST-switch and HOST-confirm, system changes with a dead man's switch";

    prefix = lib.mkOption {
      type = lib.types.str;
      default = config.networking.hostName;
      defaultText = lib.literalExpression "config.networking.hostName";
      description = "Names the commands (PREFIX-switch, PREFIX-confirm) and their units.";
    };

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
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [
      (script switch ./switch/switch.sh [ "SWITCH" "CONFIRM" "REVERT" "STATE" "FLAKE" "CONFIG" "WITHIN" ])
      (script confirm ./switch/confirm.sh [ "REVERT" "STATE" ])
    ];

    burrow.claude.machineMd = lib.mkOrder 900 ''
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
      4. Commit and push to main. main must always match what ${config.networking.hostName} runs.

      `${switch} --status` shows anything pending. `nixos-rebuild switch --rollback`
      or the boot menu goes further back.${lib.optionalString (config.systemd.settings.Manager.RuntimeWatchdogSec or null != null) " A hung box gets reset by\nthe hardware watchdog and boots the last confirmed generation."}
    '';
  };
}
