# Runners started fresh for one job each (burrow-dispatch's fresh.rs): for a
# job whose evidence has to be the job's own. A runner that takes job after
# job runs each as the same user, so any job before one, such as one a PR's
# own workflow adds, can leave a process behind that reads the next job's
# OIDC token request credentials and asks for a token in its name, or read
# the runner's own credentials and take its jobs. Each of these is
# registered for one job (GitHub's JIT config, which the machinery's App
# asks for with the organization's Self-hosted runners permission) and runs
# as a unit of its own, `burrow-fresh-runner@INSTANCE`, as a user and in a
# state directory nothing ran as or in before, removed when it stops. The
# dispatcher starts one for each job queued for their label that none idle
# is there to take, and stops one left idle. tests/fresh-runner.nix boots
# one.
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.dispatch.fresh;
  inherit (lib) mkOption types;
  inherit (config.burrow) user;
  inherit (cfg) template configs;
  state = lib.removeSuffix "@" template;
  # Its state directory by its real path. A DynamicUser's /var/lib/NAME is a
  # symlink into /var/lib/private, and a runner rooted at the symlink hands
  # its jobs a workspace path that isn't its own real one. git matches an
  # `includeIf.gitdir:` against the real path, so the credentials
  # actions/checkout includes that way never apply, and a private repo's
  # fetch asks for a password.
  root = "/var/lib/private/${state}/%i";
  start = pkgs.writeShellScript "burrow-fresh-runner" ''
    set -eu
    # The runner takes its arguments from ACTIONS_RUNNER_INPUT_* too, and
    # drops them from its environment before any job's step runs: on its
    # command line, the registration would be anyone's to read in /proc.
    ACTIONS_RUNNER_INPUT_JITCONFIG=$(<"$CREDENTIALS_DIRECTORY/jit")
    export ACTIONS_RUNNER_INPUT_JITCONFIG
    exec ${cfg.package}/bin/Runner.Listener run
  '';
  # Removes what it's given as root, from the host's mount namespace. A `+`
  # command still finds a DynamicUser's state an idmapped mount it can't
  # write (systemd 260, remi#438), and under a ProtectSystem the unit's /run
  # is read-only.
  remove = "+${pkgs.util-linux}/bin/nsenter -t 1 -m -- ${pkgs.coreutils}/bin/rm -rf";
in
{
  options.burrow.dispatch.fresh = {
    enable = lib.mkEnableOption ''
      runners started fresh for one job each, for the jobs that ask for
      their label: the machinery's App (burrow.github.machinery) needs the
      organization's Self-hosted runners permission, read and write
    '';

    owner = mkOption {
      type = types.str;
      example = "sam-org";
      description = "The organization they're registered with. Only its repos' jobs reach them.";
    };

    group = mkOption {
      type = types.ints.positive;
      default = 1;
      description = "The organization's runner group they join: 1 is its Default group.";
    };

    repos = mkOption {
      type = types.listOf types.str;
      example = [ "sam-org/homelab" ];
      description = "OWNER/REPO, whose queued jobs the dispatcher reads for their label.";
    };

    label = mkOption {
      type = types.str;
      default = "fresh-${config.networking.hostName}";
      defaultText = lib.literalExpression ''"fresh-''${config.networking.hostName}"'';
      description = ''
        The one label each is registered with. A job that asks for it and
        nothing else (`runs-on: LABEL`) is theirs.
      '';
    };

    prefix = mkOption {
      type = types.str;
      readOnly = true;
      default = "${cfg.label}-";
      defaultText = lib.literalExpression ''"''${label}-"'';
      description = ''
        What each one's name starts with, then its instance. GitHub's record
        of a job names the runner that took it, so a check that a job ran on
        one of these (Conary's train, `cn train step --fresh-runners`) asks
        for this.
      '';
    };

    most = mkOption {
      type = types.ints.positive;
      default = 2;
      description = "The most there are at once.";
    };

    package = mkOption {
      type = types.package;
      default = pkgs.github-runner;
      defaultText = lib.literalExpression "pkgs.github-runner";
      description = "The Actions runner.";
    };

    packages = mkOption {
      type = types.listOf types.package;
      default = [ ];
      description = "What their jobs find on PATH, beside git, Nix, the basics, and the jq, gh and curl burrow's shared CI calls.";
    };

    template = mkOption {
      type = types.str;
      internal = true;
      readOnly = true;
      default = "burrow-fresh-runner@";
      description = "Their unit's template: each one is TEMPLATEINSTANCE.service.";
    };

    configs = mkOption {
      type = types.str;
      internal = true;
      readOnly = true;
      default = "/run/burrow-fresh";
      description = "Where a JIT config waits for its unit to load it: the dispatcher's user's alone.";
    };
  };

  config = lib.mkIf (config.burrow.dispatch.enable && cfg.enable) {
    systemd.services.${template} = {
      description = "GitHub Actions runner %i, started fresh for one job";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      # A switch never cuts a job short; the next one starts from the new
      # unit.
      restartIfChanged = false;
      stopIfChanged = false;
      # A failed one is gone too, not left loaded for the dispatcher to
      # count, or for a switch's health check to find newly failed.
      unitConfig.CollectMode = "inactive-or-failed";
      # The basics, and what burrow's shared CI (flake-check.yml) calls, the
      # job these are for: jq, gh and curl.
      path = with pkgs; [ bash coreutils findutils gnugrep gnused gnutar gzip git config.nix.package curl gh jq ] ++ cfg.packages;
      environment = {
        RUNNER_ROOT = root;
        HOME = root;
      };
      serviceConfig = {
        # Started once its runner is: its credential is loaded by then.
        Type = "exec";
        DynamicUser = true;
        StateDirectory = "${state}/%i";
        WorkingDirectory = root;
        # The dispatcher queues the start and leaves the file to the unit,
        # which removes it once its runner has it, or as it stops if it
        # never got that far.
        LoadCredential = "jit:${configs}/%i";
        ExecStart = start;
        ExecStartPost = "${remove} ${configs}/%i";
        # Nothing it leaves is any later job's.
        ExecStopPost = "${remove} ${configs}/%i ${root} /var/lib/${state}/%i";
        Restart = "no";
        KillMode = "control-group";
        # Past a cap the kernel kills a job's biggest process, which fails
        # that step, rather than the runner.
        OOMPolicy = "continue";
        NoNewPrivileges = true;
        ProtectHome = true;
        PrivateTmp = true;
      };
    };

    systemd.tmpfiles.settings.burrow-fresh.${configs}.d = {
      mode = "0700";
      inherit user;
      group = config.users.users.${user}.group;
    };

    # The dispatcher starts and stops them, and nothing else.
    security.polkit.enable = true;
    security.polkit.extraConfig = ''
      polkit.addRule(function(action, subject) {
        if (action.id != "org.freedesktop.systemd1.manage-units" || subject.user != "${user}") return polkit.Result.NOT_HANDLED;
        var verb = action.lookup("verb");
        if (verb != "start" && verb != "stop") return polkit.Result.NOT_HANDLED;
        var unit = action.lookup("unit");
        return /^${template}[0-9]+-[0-9]+\.service$/.test(unit) ? polkit.Result.YES : polkit.Result.NOT_HANDLED;
      });
    '';
  };
}
