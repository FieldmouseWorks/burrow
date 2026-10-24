# proj: the operator hands project work to a Claude inside that project, as a
# background job (proj/proj.sh). Jobs run as burrow.user, in
# burrow.projectsDir/PROJECT, and keep everything they said in jobsDir.
# With proj.inbox, issues labelled for Claude in the projects' GitHub repos
# become jobs too (proj/inbox.sh). A job waiting on a PR doesn't hold a slot:
# proj-watch resumes it when the PR changes.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.proj;
  inherit (config.burrow) user projectsDir;

  proj = pkgs.writeShellApplication {
    name = "proj";
    runtimeInputs = [ pkgs.coreutils pkgs.findutils pkgs.gnugrep pkgs.gawk pkgs.jq pkgs.systemd pkgs.util-linux pkgs.bashInteractive ];
    text = ''
      SRC=${lib.escapeShellArg projectsDir}
      JOBS=${lib.escapeShellArg cfg.jobsDir}
      SLICE=claude-jobs.slice
      MAX_JOBS=${toString cfg.maxJobs}
      MIN_FREE=${toString cfg.minFreeMemory}
      JOB_MEMORY_MAX=${lib.escapeShellArg cfg.jobMemoryMax}
      TIMEOUT=${lib.escapeShellArg cfg.timeout}
      MAX_WAIT=${toString cfg.maxWait}
      AWAIT_HOURS=${toString cfg.awaitHours}
      GH=${lib.getExe cfg.gh}
    '' + builtins.readFile ./proj/proj.sh;
  };

  inbox = pkgs.writeShellApplication {
    name = "proj-inbox";
    runtimeInputs = [ proj pkgs.coreutils pkgs.git pkgs.gh pkgs.jq pkgs.gnused pkgs.gnugrep ];
    text = ''
      SRC=${lib.escapeShellArg projectsDir}
      JOBS=${lib.escapeShellArg cfg.jobsDir}
      STATE=${lib.escapeShellArg "${config.burrow.stateDir}/proj-inbox"}
      SCRATCH=${lib.escapeShellArg config.burrow.scratchDir}
      LABEL=${lib.escapeShellArg cfg.inbox.label}
      AUTHORS=${lib.escapeShellArg (lib.concatStringsSep " " cfg.inbox.authors)}
      VM=${if cfg.inbox.vm then "1" else "0"}
      HOST=${lib.escapeShellArg config.networking.hostName}
      NOTIFY=${lib.escapeShellArg (lib.optionalString (config.burrow.tools ? notify) (lib.getExe config.burrow.tools.notify))}
    '' + builtins.readFile ./proj/inbox.sh;
  };
in
{
  imports = [ (import ./common.nix inputs) ];

  options.burrow.proj = {
    enable = lib.mkEnableOption "proj, background Claude jobs in each project";
    jobsDir = lib.mkOption {
      type = lib.types.str;
      default = "/data/dev/jobs";
      description = "Where each job's prompts, transcript and answer are kept.";
    };
    maxJobs = lib.mkOption {
      type = lib.types.ints.positive;
      default = 4;
      description = ''
        How many jobs run Claude at once. The rest wait in line (`proj state`
        says queued), so a launcher can hand over as much work as it likes.
        Each headless Claude holds a few GB.
      '';
    };
    minFreeMemory = lib.mkOption {
      type = lib.types.ints.unsigned;
      default = 4096;
      description = ''
        MiB the machine must have to spare (available memory plus what the ZFS
        ARC would give back) before another job starts while one is running.
      '';
    };
    jobMemoryMax = lib.mkOption {
      type = lib.types.str;
      default = "8G";
      description = "The most memory one job may hold (systemd's MemoryMax) before it's killed.";
    };
    timeout = lib.mkOption {
      type = lib.types.str;
      default = "4h";
      example = "90m";
      description = ''
        How long one turn may run (coreutils timeout's units) before it's
        stopped, so a stuck job can't hold its place for good.
      '';
    };
    maxWait = lib.mkOption {
      type = lib.types.ints.positive;
      default = 1800;
      description = ''
        Seconds a queued turn waits before it counts as --first, so --first
        ones can't keep jumping ahead of it.
      '';
    };
    awaitHours = lib.mkOption {
      type = lib.types.ints.positive;
      default = 24;
      description = ''
        How long a job waits on a PR or issue (`proj await`) that doesn't
        change before it's resumed anyway, told so.
      '';
    };
    watchEvery = lib.mkOption {
      type = lib.types.str;
      default = "*:0/3";
      description = "How often proj-watch looks at what waiting jobs await (a systemd OnCalendar).";
    };
    gh = lib.mkOption {
      type = lib.types.package;
      default = pkgs.gh;
      defaultText = lib.literalExpression "pkgs.gh";
      internal = true;
      description = "The gh proj-watch asks GitHub with (the test's is a stand-in).";
    };

    inbox = {
      enable = lib.mkEnableOption ''
        proj-inbox: an open issue labelled `inbox.label` in a project's GitHub repo,
        opened by one of `inbox.authors`, becomes a proj job in that project; its
        answers go back as comments and their replies there continue it
      '';
      label = lib.mkOption {
        type = lib.types.str;
        default = "claude";
        description = "The label that hands an issue to Claude. It's created in each repo that lacks it.";
      };
      authors = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        example = [ "octocat" ];
        description = ''
          GitHub logins whose issues and replies Claude acts on. Anyone else's
          are left alone, labelled or not: an issue is a task given to an agent
          with your logins.
        '';
      };
      vm = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = "Run issue jobs in agent sandboxes (proj run --vm) instead of on the machine.";
      };
      every = lib.mkOption {
        type = lib.types.str;
        default = "*:0/3";
        description = "How often the inbox looks at the repos (a systemd OnCalendar).";
      };
    };
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [ {
    # Claude Code too, for when burrow.claude (which also installs it) is off.
    environment.systemPackages = [ proj config.burrow.claude.package ];
    burrow.tools.proj = proj;
    # Jobs are units in the user's systemd manager, so they outlive the session
    # that started them.
    users.users.${user}.linger = lib.mkDefault true;
    systemd.tmpfiles.rules = [ "d ${cfg.jobsDir} 0755 ${user} ${config.users.users.${user}.group} -" ];

    # Jobs that ended their turn waiting on GitHub (proj await) get the next
    # one when what they wait on changes, so waiting holds no slot.
    systemd.services.proj-watch = {
      description = "Resume proj jobs whose awaited PRs and issues changed";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      serviceConfig = {
        Type = "oneshot";
        User = user;
        ExecStart = "${proj}/bin/proj _watch";
      };
    };
    systemd.timers.proj-watch = {
      wantedBy = [ "timers.target" ];
      timerConfig.OnCalendar = cfg.watchEvery;
    };

    burrow.claude.machineMd = lib.mkOrder 700 ''
      ## Projects, from the operator

      ${config.burrow.owner} talks to one chat, the operator, for machine and project work alike. For
      project work, hand it to a Claude inside that project, so it gets the
      project's own CLAUDE.md and history:

          proj list                         projects in ${projectsDir}
          proj run web "the task"           start a job; prints its id
          proj run --first web "the task"   the same, ahead of the line
          proj status                       what's running, what finished
          proj log ID                       its answer (--raw for everything)
          proj reply ID "follow-up"         continue the same conversation
          proj stop ID

      Jobs run in the background and survive this session ending. At most
      ${toString cfg.maxJobs} run at once; the rest are queued. Use --first when ${config.burrow.owner} is
      waiting on the answer. Check on them,
      relay results to ${config.burrow.owner}, and follow up yourself where the answer is obvious.
      A job waiting on GitHub (CI, a review) ends its turn with `proj await
      PR-URL` and is resumed when the PR changes, holding no slot meanwhile.
      A job can't ask ${config.burrow.owner} anything: R2 commands get refused, so bring those to
      them. ${config.burrow.owner} can also open any project's own session directly from their phone.
    '';
  }

  (lib.mkIf cfg.inbox.enable {
    assertions = [{
      assertion = cfg.inbox.authors != [ ];
      message = "burrow.proj.inbox.authors: name who may hand Claude issues.";
    }];
    environment.systemPackages = [ inbox ];
    systemd.services.proj-inbox = {
      description = "Hand GitHub issues labelled ${cfg.inbox.label} to proj jobs";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      serviceConfig = {
        Type = "oneshot";
        User = user;
        ExecStart = "${inbox}/bin/proj-inbox";
      };
    };
    systemd.timers.proj-inbox = {
      wantedBy = [ "timers.target" ];
      timerConfig.OnCalendar = cfg.inbox.every;
    };
    systemd.tmpfiles.rules = [ "d ${config.burrow.stateDir}/proj-inbox 0755 ${user} ${config.users.users.${user}.group} -" ];

    burrow.claude.machineMd = lib.mkOrder 701 ''
      Issues work too: label one `${cfg.inbox.label}` in a project's GitHub repo
      (opened by ${lib.concatStringsSep " or " cfg.inbox.authors}) and within minutes it's a job there.
      The answer comes back as a comment, a reply on the issue continues the job,
      and closing it or taking the label off ends it. `proj-inbox status` shows
      which issue is which job.
    '';
  })
  ]);
}
