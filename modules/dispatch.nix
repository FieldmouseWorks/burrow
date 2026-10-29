# dispatch: what every PR waits on together, the machine's Actions runners and
# its deploy, keeps moving (crates/burrow-dispatch). The steward sees to each
# PR and the stall watchdog to each wait past its deadline; the dispatcher
# restarts a runner that has stopped taking the jobs queued for it, and
# watches what the machine hasn't put live yet. What it waits on is one of the
# watchdog's sources, so a move that didn't help goes up the watchdog's ladder
# like any other stall. It's a burrow-events handler: a runner repo's job
# events, a watched repo's push, HOST-update starting and ending, and its own
# wake-ups start its passes. No timer.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.dispatch;
  inherit (lib) mkOption types;
  inherit (config.burrow) user prefix;
  inherit (cfg) dir;
  updates = config.burrow.updates.enable or false;
  update = "${prefix}-update";
  kick = "${config.burrow.events.kick}/dispatch";

  # The commit of each watched repo the running system was built from: the
  # flake's own (dirty: none), and each of its own inputs' as its flake.lock
  # pins it (one that follows another's isn't a repo of its own).
  flakeInputs = config.burrow.switch.flakeInputs;
  lock = lib.importJSON "${flakeInputs.self}/flake.lock";
  lockedRepo = locked:
    if (locked.type or "") == "github" then "${locked.owner}/${locked.repo}"
    else let m = builtins.match "https://github.com/([^/]+)/([^/?#]+).*" (locked.url or "");
    in if m == null then null else "${lib.elemAt m 0}/${lib.removeSuffix ".git" (lib.elemAt m 1)}";
  same = a: b: a != null && lib.toLower a == lib.toLower b;
  runningOf = repo:
    if same config.burrow.updates.repo repo then flakeInputs.self.rev or null
    else let
      roots = lib.filterAttrs (_: lib.isString) lock.nodes.${lock.root}.inputs;
      named = lib.attrNames (lib.filterAttrs (_: node: same (lockedRepo (lock.nodes.${node}.locked or { })) repo) roots);
    in if named == [ ] then null else flakeInputs.${lib.head named}.rev or null;
  watched = lib.filter (r: same config.burrow.updates.repo r || runningOf r != null) config.burrow.updates.watch;
  deploying = updates && flakeInputs ? self;

  configFile = pkgs.writeText "burrow-dispatch.json" (builtins.toJSON cfg.settings);
  # The command, with this machine's config, so anyone can ask for its status
  # from any shell.
  dispatch = pkgs.writeShellScriptBin "burrow-dispatch" ''
    export DISPATCH_CONFIG=''${DISPATCH_CONFIG:-${configFile}}
    exec ${inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow}/bin/burrow-dispatch "$@"
  '';
  kickScript = pkgs.writeShellScript "burrow-dispatch-kick" ''
    ${pkgs.curl}/bin/curl -fsS -m 10 -d "" ${kick} >/dev/null || echo "couldn't kick dispatch" >&2
  '';
in
{
  imports = [
    (import ./common.nix inputs)
    { key = "burrow/events"; imports = [ (import ./events.nix inputs) ]; }
    { key = "burrow/github"; imports = [ (import ./github.nix inputs) ]; }
    { key = "burrow/switch"; imports = [ (import ./switch.nix inputs) ]; }
    { key = "burrow/watchdog/sources"; imports = [ ./watchdog/sources.nix ]; }
  ];

  options.burrow.dispatch = {
    enable = lib.mkEnableOption ''
      the pipeline dispatcher: a runner that has stopped taking the jobs
      queued for it is restarted, and what the machine hasn't put live yet is
      watched, both as stall watchdog sources
    '';

    dir = mkOption {
      type = types.str;
      default = "${config.burrow.stateDir}/dispatch";
      defaultText = lib.literalExpression ''"''${config.burrow.stateDir}/dispatch"'';
      readOnly = true;
      description = "Its state, its watch file and its wake-ups.";
    };

    runners = mkOption {
      default = { };
      example = { ci-runner = { repo = "sam/homelab"; labels = [ "homelab" ]; }; };
      description = ''
        The machine's self-hosted Actions runners, by unit: the repo each
        takes jobs for, and the labels it was registered with (beside
        self-hosted, Linux and X64, which every one has). A job only it could
        take, queued while it sits idle, is what it watches.
      '';
      type = types.attrsOf (types.submodule {
        options = {
          repo = mkOption {
            type = types.str;
            description = "OWNER/REPO.";
          };
          labels = mkOption {
            type = types.listOf types.str;
            description = "Its own labels, as `config.sh --labels` registered them.";
          };
        };
      });
    };

    settings = mkOption {
      type = types.attrs;
      internal = true;
      readOnly = true;
      default = {
        stateDir = dir;
        inherit (config.burrow.events) kick;
        watchdog = if config.burrow.watchdog.enable or false then "watchdog" else null;
        handler = "dispatch";
        inherit (cfg) runners;
        quiet = cfg.quietMinutes * 60;
        deploy = if !deploying then null else {
          unit = update;
          handler = "update";
          status = "${config.burrow.stateDir}/status.json";
          running = lib.genAttrs watched runningOf;
          after = cfg.deploy.afterMinutes * 60;
          run = cfg.deploy.runMinutes * 60;
        };
        switch = if config.burrow.switch.enable or false
          then { state = "/run/${prefix}-switch"; revert = "${prefix}-revert"; } else null;
      };
      description = "What burrow-dispatch reads (DISPATCH_CONFIG), as this machine has it.";
    };

    quietMinutes = mkOption {
      type = types.ints.positive;
      default = 5;
      description = ''
        How long a runner may sit idle with jobs queued for it before it's
        restarted: one that has lost its connection to GitHub takes nothing
        until then. Its wait goes to the watchdog after two restarts' worth.
      '';
    };

    deploy = {
      afterMinutes = mkOption {
        type = types.ints.positive;
        default = 30;
        description = ''
          How long a commit on a repo HOST-update watches (burrow.updates.watch)
          may wait to go live before it's a stall: the watchdog kicks
          HOST-update, then hands it to a Claude job.
        '';
      };
      runMinutes = mkOption {
        type = types.ints.positive;
        default = 90;
        description = "How long a run of HOST-update that has the commit may take before that's a stall.";
      };
    };
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [ {
    environment.systemPackages = [ dispatch ];

    burrow.events.handlers.dispatch = {
      # A runner repo's jobs queued, started and done; a watched repo's main
      # moving.
      events = [ "workflow_job" "push" ];
      repos = lib.unique (lib.mapAttrsToList (_: r: r.repo) cfg.runners ++ lib.optionals deploying watched);
      pushes = "default";
      perRepo = false;
      unit = "burrow-dispatch";
      wake = "${dir}/wake.json";
    };
    systemd.services.burrow-dispatch = {
      description = "burrow-dispatch: keep the runners and the deploy every PR waits on moving";
      # gh as the machinery's App, or the user's own login without one.
      path = [ config.burrow.github.machineGh config.systemd.package ];
      serviceConfig = {
        Type = "oneshot";
        # The watch files it reads are the user's, and so is the stall
        # watchdog that reads its own.
        User = user;
        ExecStart = "${dispatch}/bin/burrow-dispatch";
        TimeoutStartSec = "5min";
      };
    };
    systemd.tmpfiles.settings.burrow.${dir}.d = { mode = "0755"; inherit user; group = config.users.users.${user}.group; };

    # It restarts and starts the runners, and nothing else.
    security.polkit.enable = true;
    security.polkit.extraConfig = ''
      polkit.addRule(function(action, subject) {
        if (action.id != "org.freedesktop.systemd1.manage-units" || subject.user != "${user}") return polkit.Result.NOT_HANDLED;
        var verb = action.lookup("verb");
        if (verb != "restart" && verb != "start") return polkit.Result.NOT_HANDLED;
        var units = ${builtins.toJSON (map (u: "${u}.service") (lib.attrNames cfg.runners))};
        return units.indexOf(action.lookup("unit")) >= 0 ? polkit.Result.YES : polkit.Result.NOT_HANDLED;
      });
    '';

    burrow.watchdog.sources.dispatch = {
      file = "${dir}/watch.json";
      about = "The pipeline dispatcher (burrow-dispatch)";
      look = "`burrow-dispatch status` shows each runner, the jobs queued for it and whether it's at one, each watched repo's commits not yet live, and the moves it has made; `journalctl -u burrow-dispatch`, `journalctl -u RUNNER` and `${update} status` say more.";
      kick = "dispatch";
      # It writes on every pass, and has one at least hourly (burrow-events'
      # catch-up).
      staleMinutes = 120;
    };

    burrow.claude.machineMd = lib.mkOrder 1065 ''
      ## Runners and deploys

      `burrow-dispatch` restarts an Actions runner (${lib.concatStringsSep ", " (lib.attrNames cfg.runners)}) that sits idle
      with jobs queued for it, at most twice in six hours, and watches commits ${config.networking.hostName} hasn't put
      live yet; both go to the stall watchdog when they last. `burrow-dispatch status` shows what it sees and did.
    '';
  } (lib.mkIf deploying {
    # HOST-update starting and ending moves what the deploy waits on.
    systemd.services.${update}.serviceConfig = {
      ExecStartPre = [ "-${kickScript}" ];
      ExecStopPost = [ "-${kickScript}" ];
    };
  }) ]);
}
