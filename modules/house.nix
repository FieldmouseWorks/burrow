# house: every repo's copies of the house blocks (the files in house/, each
# between its markers in AGENTS.md) kept the same as burrow's (crates/house-sync).
# A change to a block here reaches each repo as one PR, which the steward
# lands: the machine's own App's (burrow.github.machinery), when it has one.
# Nothing waits for a timer: a switch that brings new blocks starts a pass
# over every repo, and a push to a repo's default branch one over that repo.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.house;
  inherit (config.burrow) user;
  inherit (lib) mkOption types;
  # The blocks the running system has: /etc moves to a switch's new ones
  # before it starts anything, so a pass started under older blocks can tell.
  current = "burrow/house";
  lockDir = "/run/house-sync";

  # What a pass is told (crates/house-sync), and the command with it built
  # in. gh is the machine's: as its own App, when it has one.
  syncConfig = pkgs.writeText "house-sync.json" (builtins.toJSON {
    house = ../house;
    current = "/etc/${current}";
    lock = "${lockDir}/lock";
    inherit (cfg) owners exclude dryRun;
    bash = lib.getExe pkgs.bash;
    notify = "${config.burrow.tools.notify}/bin/${config.burrow.prefix}-notify";
  });
  sync = pkgs.runCommand "house-sync" {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = "house-sync";
  } ''
    makeBinaryWrapper ${inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow}/bin/house-sync \
      $out/bin/house-sync --add-flags ${syncConfig} \
      --prefix PATH : ${lib.makeBinPath [ config.burrow.github.machineGh ]}
  '';
in
{
  imports = [
    (import ./common.nix inputs)
    { key = "burrow/notify"; imports = [ (import ./notify.nix inputs) ]; }
    { key = "burrow/events"; imports = [ (import ./events.nix inputs) ]; }
    { key = "burrow/github"; imports = [ (import ./github.nix inputs) ]; }
  ];

  options.burrow.house = {
    enable = lib.mkEnableOption "house-sync, every repo's house blocks kept current";

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
        Repos to leave alone (OWNER/NAME patterns, as bash's `[[ == ]]` reads them). Not autoland's exclude: a repo
        that lands its own PRs still keeps the house blocks, and gets its
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
    environment.etc.${current}.source = ../house;
    # Every pass, full or one repo's, takes this lock: burrow-events lines
    # them up, but not across its own restarts, and two passes at once can
    # each close the other's PR.
    systemd.tmpfiles.rules = [ "d ${lockDir} 0755 ${user} ${config.users.users.${user}.group} -" ];

    # A push to a repo's default branch (a house PR merged, AGENTS.md edited
    # by hand, a new repo's first commits) starts house-sync@OWNER-REPO, its
    # pass over that repo. burrow-events' catch-up, when it starts and hourly,
    # is the full pass, for what no push reports: a house PR closed unmerged.
    # New blocks need nothing of their own: the units carry them (sync's
    # HOUSE), so a switch that brings some changes the units, and
    # burrow-events' switch kick for a handler whose units changed starts the
    # full pass.
    burrow.events.handlers.house = {
      events = [ "push" ];
      unit = "house-sync";
      pushes = "default";
    };
    systemd.services = lib.genAttrs [ "house-sync" "house-sync@" ] (name: {
      description = "house-sync: ${if name == "house-sync" then "every repo's" else "%I's"} house blocks, as burrow has them";
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
    });

    burrow.claude.machineMd = lib.mkOrder 1060 ''
      ## The house blocks

      The house blocks in every repo's AGENTS.md (the house rules, and the
      Pull requests section) are copies of the files in burrow's house/, and
      `house-sync` (on new blocks, and each repo's pushes to its default
      branch) opens a PR wherever a copy differs. To change one, change it in
      burrow, never between a repo's markers.
    '';
  };
}
