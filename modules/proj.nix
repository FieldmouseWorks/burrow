# proj: the operator hands project work to an agent inside that project, as a
# background job (crates/burrow-proj). Jobs run as burrow.user, in
# burrow.projectsDir/PROJECT, and keep everything they said in jobsDir. Each
# makes its worktrees in a scratch dir of its own under burrow.scratchDir,
# cleared once the job is over (its scratch.rs).
# With proj.inbox, issues labelled for an engine in the projects' GitHub repos
# become jobs too (proj-inbox, in the same crate). A job waiting on a PR
# doesn't hold a slot: proj-watch resumes it when the PR changes. Both are
# burrow.events handlers: a repo's webhook deliveries, the end of a turn and
# their own wake-ups start them, never a timer. With the stall watchdog on,
# proj is one of its sources: each job's wait, as of its last change.
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
  # The pipeline's own repos, whose projects' pipeline jobs get its slots,
  # and the lead ones, what it's built from, whose go first of all (pipeline
  # repos whether or not pipelineRepos lists them).
  leadRepos = lib.attrByPath [ "burrow" "autoland" "leadRepos" ] [ ] config;
  pipelineRepos = lib.attrByPath [ "burrow" "autoland" "pipelineRepos" ] [ ] config ++ leadRepos;
  # What each job waits on, for the stall watchdog, when it's on.
  watchdog = config.burrow.watchdog.enable or false;
  waits = "${cfg.jobsDir}/.waits.json";

  agents = import ./autoland/agents/package.nix { inherit pkgs; };
  package = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-proj;
  hooks = pkgs.runCommand "proj-codex-git-hooks" { } ''
    mkdir -p $out
    ln -s ${package}/bin/burrow-proj-git-hook $out/commit-msg
    ln -s ${package}/bin/burrow-proj-git-hook $out/pre-push
  '';
  codexHome = "${config.users.users.${user}.home}/.codex";
  guard = import ./claude/guard.nix { inherit inputs pkgs lib config; };
  policy = import ./claude/codex-rules.nix {
    inherit lib;
    patterns = (import ./claude/r2.nix) ++ config.burrow.claude.extraAsk;
  };
  rulesFile = pkgs.writeText "proj-codex.rules" policy.text;
  instructions = pkgs.writeText "proj-AGENTS.md" (config.burrow.claude.machineMd + ''

    Codex jobs run headless with network access, in a sandbox that writes only
    the project, $PROJ_SCRATCH, the job's own dir and git's metadata, and
    hides Codex's login, the user's runtime dir (its D-Bus and systemd
    manager: no systemctl --user or systemd-run --user) and tmux's servers. The
    sandbox's no_new_privs prevents sudo elevation. One-way commands are forbidden; bring
    them to the owner. The machine's guard checks every command and patch first:
    what deletes or rewrites files gets an undo snapshot, or is refused without
    one. Commits carry Engine: codex, and a Claude judge decides whether their PR
    lands after the usual Codex review and checks; so the guard refuses what
    would turn off your git hooks or write commits they don't see (revert,
    cherry-pick, rebase, am, commit-tree and other plumbing, git aliases,
    GitHub's API, gh pr merge): commit and merge instead, and leave merging
    PRs to the steward. There are no TTY sessions: run each command whole.
  '');

  configFile = pkgs.writeText "proj.json" (builtins.toJSON {
    projects = projectsDir;
    jobs = cfg.jobsDir;
    scratch = config.burrow.scratchDir;
    slice = "claude-jobs.slice";
    inherit (cfg) maxJobs pipelineSlots maxJobsPerProject minFreeMemory jobMemoryMax timeout maxWait stallAfterMinutes awaitHours;
    inherit (config.burrow.events) kick;
    wake = watchWake;
    waits = if watchdog then waits else null;
    inherit inboxMark pipelineRepos leadRepos;
    inherit (config.burrow) flake;
    deployed = "/run/current-system/sw/bin/proj";
    apps = lib.mapAttrs (_: app: app.envFile) config.burrow.github.apps;
    notify = if config.burrow.tools ? notify then lib.getExe config.burrow.tools.notify else null;
    codexSlots = cfg.codex.slots;
    codexHooks = hooks;
    meter = cfg.meter;
    roles = "${config.users.users.${user}.home}/.config/proj/roles";
    inbox = if cfg.inbox.enable then {
      state = "${stateDir}/proj-inbox";
      inherit (cfg.inbox) labels unstickLabel authors vm;
      host = config.networking.hostName;
      wake = inboxWake;
      machinery = if machineApp == null then null else machineApp.name;
    } else null;
    tools = {
      codex = "${agents}/bin/codex";
      # A login shell's, for the jobs' Claude.
      bash = "${pkgs.bashInteractive}/bin/bash";
      flock = "${pkgs.util-linux}/bin/flock";
      gh = lib.getExe cfg.gh;
      git = lib.getExe config.programs.git.package;
      systemctl = "${pkgs.systemd}/bin/systemctl";
      systemdRun = "${pkgs.systemd}/bin/systemd-run";
      timeout = "${pkgs.coreutils}/bin/timeout";
    };
  });
  # The command, with this machine's config. Always this one: a turn that
  # outlived a switch runs the binary it started with, and its Claude's proj
  # calls get the new one with its own config, never the old. PROJ_SELF is how
  # it knows which it is: this script, not the binary, since a switch can change
  # the config alone (slot.rs, the hand-over).
  proj = pkgs.writeShellScriptBin "proj" ''
    export PROJ_CONFIG=${configFile} PROJ_SELF=$0
    exec ${inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-proj}/bin/burrow-proj "$@"
  '';

  # proj-inbox, with the same config, whose inbox part is its own. The turns
  # it starts are proj's (PROJ_SELF), as if a job's Claude had run proj.
  inbox = pkgs.writeShellScriptBin "proj-inbox" ''
    export PROJ_CONFIG=${configFile} PROJ_SELF=${proj}/bin/proj
    exec ${inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-proj}/bin/burrow-proj-inbox "$@"
  '';
in
{
  imports = [
    (import ./common.nix inputs)
    { key = "burrow/codex-login"; imports = [ (import ./codex-login.nix inputs) ]; }
    { key = "burrow/claude"; imports = [ (import ./claude.nix inputs) ]; }
    { key = "burrow/events"; imports = [ (import ./events.nix inputs) ]; }
    { key = "burrow/watchdog/sources"; imports = [ ./watchdog/sources.nix ]; }
    { key = "burrow/github"; imports = [ (import ./github.nix inputs) ]; }
  ];

  options.burrow.proj = {
    enable = lib.mkEnableOption "proj, background agent jobs in each project";
    codex.slots = lib.mkOption {
      type = lib.types.ints.positive;
      default = 2;
      description = "How many Codex jobs may run at once, in a pool of their own. Keep it small: the plan's PR reviewer comes first.";
    };
    meter = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Claude's weekly meter.json from usage-reset. Role overflow uses readings under an hour old; null disables it.";
    };
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
        Each headless Claude holds a few GB. One more, run `--unstick` (the
        stuck issue's, `inbox.unstickLabel`), has a slot of its own besides.
      '';
    };
    pipelineSlots = lib.mkOption {
      type = lib.types.ints.unsigned;
      default = if cfg.maxJobs > 1 then 1 else 0;
      defaultText = lib.literalExpression "if maxJobs > 1 then 1 else 0";
      description = ''
        Of the maxJobs slots, how many only pipeline jobs in the pipeline's
        own projects (`proj run --pipeline` in the machine's flake,
        `burrow.flake`, or in a project whose clone's origin is one of
        `burrow.autoland.pipelineRepos` or `leadRepos` on GitHub: the update
        job's fixes, the steward's jobs there, their issues) may take, so
        work on the pipeline itself waits only on other such work, never on
        everyone else's long jobs; a lead repo's go first of all, ahead of
        the other pipeline jobs too, but leave them these slots while one
        waits. All of them leave everyone else one of the other slots while
        one of theirs waits and none runs. A pipeline job in any other project is a
        steward job: ahead of the line, but within its project's cap and
        never in these slots; `proj status` and its turn's journal say so,
        origin and all.
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
        project's jobs never fill the machine. The last of these stays free
        for the project's steward jobs (`proj run --pipeline`, there a pipeline
        job goes within this cap): its other jobs, `--first` ones and ones
        that waited `maxWait` included, hold at most maxJobsPerProject - 1,
        or the one slot when this is 1, so its own lanes can't hold up its
        gates and conflict fixes. Steward jobs may use the whole share.
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
      default = "infinity";
      example = "8G";
      description = ''
        The most memory one job may hold (systemd's MemoryMax). None by
        default: a job is the first thing the kernel kills when the machine
        runs short (OOMScoreAdjust 500), and systemd-oomd watches the jobs'
        slice (burrow.ops.oomd), so a cap only kills one while memory sits
        free. With one, past it the kernel kills the job's biggest process,
        usually a build or evaluation it ran, and the job carries on, told
        that command was killed.
      '';
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
        proj-inbox: an open issue carrying a label in `inbox.labels` in a project's GitHub repo,
        opened by one of `inbox.authors`, becomes a proj job in that project; its
        answers go back as comments and their replies there continue it
      '';
      labels = lib.mkOption {
        type = lib.types.attrsOf (lib.types.enum [ "claude" "codex" ]);
        default = { claude = "claude"; codex = "codex"; };
        description = "Issue labels and the engine each starts. Codex takes code with a checkable bar; Claude takes judgment and what the owner reads. Each label is created in each repo; Codex wins if an issue has both.";
      };
      unstickLabel = lib.mkOption {
        type = lib.types.str;
        default = "unstick";
        description = ''
          An inbox issue with this label too is about what the pipeline is
          stuck on (the steward's stuck issue has it): its job runs
          `proj run --unstick`, in a slot of its own past maxJobs, so it never
          waits in the line it may be about. Put on an issue whose job is
          still waiting for its first turn, it starts that job over that way.
          Codex jobs still stay within their own codex.slots cap.
        '';
      };
      authors = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        example = [ "octocat" ];
        description = ''
          GitHub logins whose issues and replies the agents act on. Anyone else's
          are left alone, labelled or not: an issue is a task given to an agent
          with your logins.
        '';
      };
      vm = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = "Run issue jobs in Claude agent sandboxes (proj run --vm). A Codex selection fails its turn.";
      };
    };
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [ {
    assertions = [{
      assertion = cfg.pipelineSlots < cfg.maxJobs;
      message = "burrow.proj.pipelineSlots: leave other jobs at least one of the maxJobs slots.";
    }];
    # Claude Code too, for when burrow.claude (which also installs it) is off.
    environment.systemPackages = [ proj config.burrow.claude.package agents ];
    burrow.tools.proj = proj;
    # The guard is Codex's managed hook for the jobs, wherever proj runs.
    burrow.tools.codex-guard = guard.codex;
    environment.etc."codex/requirements.toml".source = guard.codexRequirements;
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
    # The end of each job's turn (proj _ended), which each turn's unit starts
    # as it goes: in a unit of its own, so what kills the turn's whole cgroup
    # (systemd-oomd) leaves it be. The job comes in its environment, from the
    # instance name: a job's id is its project's folder name, spaces, `%` and
    # all.
    systemd.user.services."proj-ended@" = {
      description = "The end of a turn of proj job %I";
      unitConfig.ConditionUser = user;
      environment.PROJ_ENDED = "%I";
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${proj}/bin/proj _ended";
      };
    };
    # Per-user instructions and policy follow the same options as Claude's.
    systemd.tmpfiles.rules = [
      "d ${codexHome} 0700 ${user} ${config.users.users.${user}.group} -"
      "d ${codexHome}/rules 0700 ${user} ${config.users.users.${user}.group} -"
      "L+ ${codexHome}/AGENTS.md - - - - ${instructions}"
      "L+ ${codexHome}/rules/burrow.rules - - - - ${rulesFile}"
      "d ${cfg.jobsDir} 0755 ${user} ${config.users.users.${user}.group} -"
      "d ${stateDir}/proj-watch 0755 ${user} ${config.users.users.${user}.group} -"
      # Each job's scratch (burrow-proj's scratch.rs), which it clears itself once the
      # job is over: no age here.
      "d ${config.burrow.scratchDir}/jobs 0755 ${user} ${config.users.users.${user}.group} -"
    ];

    # What every job waits on, for the stall watchdog: proj rewrites it, and
    # kicks the watchdog, each time a job's wait changes, so an old file is
    # no news.
    burrow.watchdog.sources.proj = {
      file = waits;
      about = "proj, the machine's Claude jobs";
      look = "`proj status` lists the jobs, and `proj log ID` (with --raw, its whole stream) shows what one did and where it stopped; `proj reply ID \"...\"` gives it its next turn, and `proj stop ID` ends one that's finished or no longer wanted.";
    };

    burrow.claude.codeMd = ''
      A Claude job has the machine's MCP servers but not the claude.ai connectors
      (mail, calendar, drive) or skills: work that needs those is a session's.
    '';
    burrow.claude.machineMd = lib.mkOrder 700 ''
      ## Projects, from the operator

      ${config.burrow.owner} talks to one chat, the operator, for machine and project work alike. For
      project work, hand it to a Claude inside that project, so it gets the
      project's own instructions and history:

          proj list                         projects in ${projectsDir}
          proj run web "the task"           start a job; prints its id
          proj run --first web "the task"   the same, ahead of the line
          proj run --pipeline burrow "..."  work on the pipeline itself: first of all, in its own slot
          proj run --app NAME web "task"    acting on GitHub as the App NAME
          proj run --role NAME web "task"   on the engine ~/.config/proj/roles gives NAME
          proj status                       what's running, what finished
          proj status --line                every turn in line, next first, and its wait
          proj log ID                       its answer (--turn N for an earlier one, --raw for everything)
          proj reply ID "follow-up"         continue the same conversation
          proj stop ID                      end it: its running turn, and anything it awaits

      Jobs run in the background and survive this session ending. At most
      ${toString cfg.maxJobs} run at once, ${toString cfg.maxJobsPerProject} from any one project; the rest are queued. Use --first when ${config.burrow.owner} is
      waiting on the answer, and --pipeline for work on the steward, its reviewers, CI or proj${lib.optionalString (leadRepos != [ ]) " (in ${lib.concatStringsSep ", " leadRepos}, what the pipeline is built from, it goes first of all)"}. Check on them,
      relay results to ${config.burrow.owner}, and follow up yourself where the answer is obvious.
      Each turn ends saying what comes next: `proj await PR-URL` when it waits
      on GitHub (CI, a review; it's resumed when the PR changes, holding no slot
      meanwhile), `proj ask` when its answer is a question for you to settle or
      relay (`proj status` marks it [asks]; `proj reply` answers), `proj done`
      when it's finished. One that won't say is marked [no next step]${lib.optionalString watchdog ", and the stall watchdog takes it"}.
      Roles live in ~/.config/proj/roles, one `ROLE ENGINE[:MODEL]` line each.
      `ROLE@OWNER/REPO` takes precedence in that repo, matched case-insensitively;
      other repos and jobs with no repo use the bare role. Duplicate keys fail
      the turn with both lines named. Engines are claude and codex; unlisted
      roles use Claude. Either kind of line may end with `over PCT ENGINE[:MODEL]`
      for a fresh weekly meter at that percent.
      Codex has ${toString cfg.codex.slots} slots of its own. Log the job user in with
      `autoland-codex-login job`, separately from the reviewer's login. Codex
      jobs cannot run in agent VMs. The steward's judge always uses Claude.
      A job can't ask ${config.burrow.owner} anything: R2 commands get refused, so bring those to
      them. ${config.burrow.owner} can also open any project's own session directly from their phone.
    '';
  }

  (lib.mkIf cfg.inbox.enable {
    assertions = [{
      assertion = cfg.inbox.authors != [ ];
      message = "burrow.proj.inbox.authors: name who may hand the agents issues.";
    }];
    environment.systemPackages = [ inbox ];
    # The steward has it forget a repo GitHub's name now means another of.
    burrow.tools.proj-inbox = inbox;
    # A repo's issue events start its look (proj-inbox@OWNER-REPO), and so
    # do the start and end of each of its jobs' turns (proj run --kick): it
    # posts the answer once one has ended.
    burrow.events.handlers.proj-inbox = {
      events = [ "issues" "issue_comment" ];
      unit = "proj-inbox";
      wake = inboxWake;
    };
    systemd.services = lib.genAttrs [ "proj-inbox" "proj-inbox@" ] (name: {
      description = "Hand GitHub issues labelled ${lib.concatStringsSep ", " (lib.attrNames cfg.inbox.labels)} to proj jobs${lib.optionalString (name == "proj-inbox@") " in %I"}";
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
      Issues work too: label one `${lib.concatStringsSep "` or `" (lib.attrNames cfg.inbox.labels)}` in a project's GitHub repo
      (opened by ${lib.concatStringsSep " or " cfg.inbox.authors}) and within seconds it's a job there.
      The answer comes back as a comment, a reply on the issue continues the job,
      and closing it or taking the label off ends it. `proj-inbox status` shows
      which issue is which job.${lib.optionalString (machineApp != null) " A person's issue's job acts on GitHub as
      them; a bot's acts as the App ${machineApp.slug}."}
    '';
  })
  ]);
}
