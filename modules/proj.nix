# proj: the operator hands project work to a Claude inside that project, as a
# background job (proj/proj.sh). Jobs run as burrow.user, in
# burrow.projectsDir/PROJECT, and keep everything they said in jobsDir.
# With proj.inbox, issues labelled for Claude in the projects' GitHub repos
# become jobs too (proj/inbox.sh). A job waiting on a PR doesn't hold a slot:
# proj-watch resumes it when the PR changes. Both are burrow.events handlers:
# a repo's webhook deliveries, the end of a turn and their own wake-ups start
# them, never a timer. With the stall watchdog on, proj is one of its
# sources: each job's wait, as of its last change (proj/waits.sh).
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.proj;
  # What marks proj-inbox's own comments on an issue: never a reply, never news.
  inboxMark = "<!-- proj-inbox -->";
  inherit (config.burrow) user projectsDir stateDir;
  inherit (config.burrow.github) machineApp;
  watchWake = "${stateDir}/proj-watch/wake.json";
  inboxWake = "${stateDir}/proj-inbox/wake.json";
  # The pipeline's own repos, whose projects' pipeline jobs get its slots.
  pipelineRepos = lib.escapeShellArg (lib.concatStringsSep " " (lib.attrByPath [ "burrow" "autoland" "pipelineRepos" ] [ ] config));
  # What each job waits on, for the stall watchdog, when it's on.
  watchdog = config.burrow.watchdog.enable or false;
  waits = "${cfg.jobsDir}/.waits.json";

  proj = pkgs.writeShellApplication {
    name = "proj";
    runtimeInputs = [ pkgs.coreutils pkgs.findutils pkgs.gnugrep pkgs.gnused pkgs.gawk pkgs.jq pkgs.curl pkgs.git pkgs.systemd pkgs.util-linux pkgs.bashInteractive ];
    text = ''
      SRC=${lib.escapeShellArg projectsDir}
      JOBS=${lib.escapeShellArg cfg.jobsDir}
      SLICE=claude-jobs.slice
      MAX_JOBS=${toString cfg.maxJobs}
      PIPELINE_SLOTS=${toString cfg.pipelineSlots}
      PER_PROJECT=${toString cfg.maxJobsPerProject}
      MIN_FREE=${toString cfg.minFreeMemory}
      JOB_MEMORY_MAX=${lib.escapeShellArg cfg.jobMemoryMax}
      TIMEOUT=${lib.escapeShellArg cfg.timeout}
      MAX_WAIT=${toString cfg.maxWait}
      AWAIT_HOURS=${toString cfg.awaitHours}
      GH=${lib.getExe cfg.gh}
      MARK=${lib.escapeShellArg inboxMark}
      KICK=${lib.escapeShellArg config.burrow.events.kick}
      WAKE=${lib.escapeShellArg watchWake}
      PIPELINE=${pipelineRepos}
      WAITS=${lib.escapeShellArg (lib.optionalString watchdog waits)}
      STALL_QUEUED=${toString (cfg.stallAfterMinutes * 60)}
      GRACE=600
      APPS=${lib.escapeShellArg (lib.concatStrings (lib.mapAttrsToList (name: app: "${name}=${app.envFile}\n") config.burrow.github.apps))}
    '' + builtins.readFile ./proj/pick.sh + builtins.readFile ./proj/waits.sh + builtins.readFile ./proj/proj.sh;
  };

  inbox = pkgs.writeShellApplication {
    name = "proj-inbox";
    runtimeInputs = [ proj pkgs.coreutils pkgs.git cfg.gh pkgs.jq pkgs.gnused pkgs.gnugrep pkgs.util-linux ];
    text = ''
      SRC=${lib.escapeShellArg projectsDir}
      JOBS=${lib.escapeShellArg cfg.jobsDir}
      STATE=${lib.escapeShellArg "${stateDir}/proj-inbox"}
      SCRATCH=${lib.escapeShellArg config.burrow.scratchDir}
      LABEL=${lib.escapeShellArg cfg.inbox.label}
      AUTHORS=${lib.escapeShellArg (lib.concatStringsSep " " cfg.inbox.authors)}
      VM=${if cfg.inbox.vm then "1" else "0"}
      MARK=${lib.escapeShellArg inboxMark}
      HOST=${lib.escapeShellArg config.networking.hostName}
      NOTIFY=${lib.escapeShellArg (lib.optionalString (config.burrow.tools ? notify) (lib.getExe config.burrow.tools.notify))}
      WAKE=${lib.escapeShellArg inboxWake}
      PIPELINE=${pipelineRepos}
      MACHINERY=${lib.escapeShellArg (if machineApp == null then "" else machineApp.name)}
    '' + builtins.readFile ./proj/inbox.sh;
  };
in
{
  imports = [
    (import ./common.nix inputs)
    { key = "burrow/events"; imports = [ (import ./events.nix inputs) ]; }
    { key = "burrow/watchdog/sources"; imports = [ ./watchdog/sources.nix ]; }
    { key = "burrow/github"; imports = [ (import ./github.nix inputs) ]; }
  ];

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
    pipelineSlots = lib.mkOption {
      type = lib.types.ints.unsigned;
      default = if cfg.maxJobs > 1 then 1 else 0;
      defaultText = lib.literalExpression "if maxJobs > 1 then 1 else 0";
      description = ''
        Of the maxJobs slots, how many only pipeline jobs in the pipeline's
        own repos (`proj run --pipeline` in a project whose clone's origin
        is one of `burrow.autoland.pipelineRepos` on GitHub: the steward's
        jobs there, their issues) may take, so work on the pipeline itself
        waits only on other such work, never on everyone else's long jobs. A
        pipeline job in any other project goes as `--first`: ahead of the
        line, but within its project's cap and never in these slots; `proj
        status` and its turn's journal say so, origin and all.
      '';
    };
    maxJobsPerProject = lib.mkOption {
      type = lib.types.ints.positive;
      default = lib.max 1 (cfg.maxJobs - cfg.pipelineSlots - 1);
      defaultText = lib.literalExpression "max 1 (maxJobs - pipelineSlots - 1)";
      description = ''
        The most slots one project's jobs hold at once, the pipeline's own
        repos' pipeline jobs aside.
        Its next one waits, and lets other projects' by meanwhile, so one
        project's jobs never fill the machine.
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
        stopped, so a stuck job can't hold its place for good. A job can set
        its own (`proj run --timeout`), and stop sooner once it has gone quiet
        (`--idle`), as the steward's do (`burrow.autoland.jobTimeoutMinutes`,
        `jobIdleMinutes`). Time a turn spends waiting in line inside its run
        doesn't count: each turn's commands get PROJ_WAIT_DIR, where one that
        waits for something shared (a machine-wide lock) keeps a file, locked,
        saying what for, while it waits; the stall watchdog holds that wait
        to stallAfterMinutes instead.
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
    stallAfterMinutes = lib.mkOption {
      type = lib.types.ints.positive;
      default = 120;
      description = ''
        How long a turn may wait in line, for a slot or inside its run (a
        command of its holding a file in its PROJ_WAIT_DIR: its timeout
        stands still meanwhile), before the stall watchdog, when it's on,
        takes it. A turn running past its timeout, one awaiting GitHub past
        awaitHours, or one that ended without saying what comes next, it
        takes then.
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
    gh = lib.mkOption {
      type = lib.types.package;
      default = config.burrow.github.machineGh;
      defaultText = lib.literalExpression "config.burrow.github.machineGh";
      internal = true;
      description = "The gh proj-watch and proj-inbox ask GitHub with (the test's is a stand-in).";
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
    };
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [ {
    assertions = [{
      assertion = cfg.pipelineSlots < cfg.maxJobs;
      message = "burrow.proj.pipelineSlots: leave other jobs at least one of the maxJobs slots.";
    }];
    # Claude Code too, for when burrow.claude (which also installs it) is off.
    environment.systemPackages = [ proj config.burrow.claude.package ];
    burrow.tools.proj = proj;
    # Jobs are units in the user's systemd manager, so they outlive the session
    # that started them.
    users.users.${user}.linger = lib.mkDefault true;

    # Jobs that ended their turn waiting on GitHub (proj await) get the next
    # one when what they wait on changes, so waiting holds no slot. A PR's or
    # issue's events start the look at its repo (proj-watch@OWNER-REPO), and so
    # does the end of a turn that awaits it; the deadline is booked in its
    # wake file.
    burrow.events.handlers.proj-watch = {
      events = [ "pull_request" "pull_request_review" "pull_request_review_comment" "issue_comment" "issues" "check_suite" "status" ];
      unit = "proj-watch";
      wake = watchWake;
    };
    systemd.services = lib.genAttrs [ "proj-watch" "proj-watch@" ] (name: {
      description = "Resume proj jobs whose awaited PRs and issues changed${lib.optionalString (name == "proj-watch@") " in %I"}";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      serviceConfig = {
        Type = "oneshot";
        User = user;
        ExecStart = "${proj}/bin/proj _watch${lib.optionalString (name == "proj-watch@") " %I"}";
      };
    });
    systemd.tmpfiles.rules = [
      "d ${cfg.jobsDir} 0755 ${user} ${config.users.users.${user}.group} -"
      "d ${stateDir}/proj-watch 0755 ${user} ${config.users.users.${user}.group} -"
    ];

    # What every job waits on, for the stall watchdog: proj rewrites it, and
    # kicks the watchdog, each time a job's wait changes, so an old file is
    # no news.
    burrow.watchdog.sources.proj = {
      file = waits;
      about = "proj, the machine's Claude jobs";
      look = "`proj status` lists the jobs, and `proj log ID` (with --raw, its whole stream) shows what one did and where it stopped; `proj reply ID \"...\"` gives it its next turn, and `proj stop ID` ends one that's finished or no longer wanted.";
    };

    burrow.claude.machineMd = lib.mkOrder 700 ''
      ## Projects, from the operator

      ${config.burrow.owner} talks to one chat, the operator, for machine and project work alike. For
      project work, hand it to a Claude inside that project, so it gets the
      project's own CLAUDE.md and history:

          proj list                         projects in ${projectsDir}
          proj run web "the task"           start a job; prints its id
          proj run --first web "the task"   the same, ahead of the line
          proj run --pipeline burrow "..."  work on the pipeline itself: first of all, in its own slot
          proj run --app NAME web "task"    acting on GitHub as the App NAME
          proj status                       what's running, what finished
          proj log ID                       its answer (--turn N for an earlier one, --raw for everything)
          proj reply ID "follow-up"         continue the same conversation
          proj stop ID                      end it: its running turn, and anything it awaits

      Jobs run in the background and survive this session ending. At most
      ${toString cfg.maxJobs} run at once, ${toString cfg.maxJobsPerProject} from any one project; the rest are queued. Use --first when ${config.burrow.owner} is
      waiting on the answer, and --pipeline for work on the steward, its reviewers, CI or proj. Check on them,
      relay results to ${config.burrow.owner}, and follow up yourself where the answer is obvious.
      Each turn ends saying what comes next: `proj await PR-URL` when it waits
      on GitHub (CI, a review; it's resumed when the PR changes, holding no slot
      meanwhile), `proj ask` when its answer is a question for you to settle or
      relay (`proj status` marks it [asks]; `proj reply` answers), `proj done`
      when it's finished. One that won't say is marked [no next step]${lib.optionalString watchdog ", and the stall watchdog takes it"}.
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
    # A repo's issue events start its look (proj-inbox@OWNER-REPO), and so
    # does the end of each of its jobs' turns (proj run --kick), whose answer
    # it posts.
    burrow.events.handlers.proj-inbox = {
      events = [ "issues" "issue_comment" ];
      unit = "proj-inbox";
      wake = inboxWake;
    };
    systemd.services = lib.genAttrs [ "proj-inbox" "proj-inbox@" ] (name: {
      description = "Hand GitHub issues labelled ${cfg.inbox.label} to proj jobs${lib.optionalString (name == "proj-inbox@") " in %I"}";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      serviceConfig = {
        Type = "oneshot";
        User = user;
        ExecStart = "${inbox}/bin/proj-inbox${lib.optionalString (name == "proj-inbox@") " %I"}";
      };
    });
    systemd.tmpfiles.rules = [ "d ${stateDir}/proj-inbox 0755 ${user} ${config.users.users.${user}.group} -" ];

    burrow.claude.machineMd = lib.mkOrder 701 ''
      Issues work too: label one `${cfg.inbox.label}` in a project's GitHub repo
      (opened by ${lib.concatStringsSep " or " cfg.inbox.authors}) and within seconds it's a job there.
      The answer comes back as a comment, a reply on the issue continues the job,
      and closing it or taking the label off ends it. `proj-inbox status` shows
      which issue is which job.${lib.optionalString (machineApp != null) " A person's issue's job acts on GitHub as
      them; a bot's acts as the App ${machineApp.slug}."}
    '';
  })
  ]);
}
