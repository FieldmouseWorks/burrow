# autoland: every PR in every repo you own goes from ready to merged on its
# own, once a reviewer has nothing left to say (autoland/steward.py).
#
#   ready -> Codex reviews it (an agent reading a snapshot of the head, as
#            a user of its own with no GitHub access)
#         -> Claude fixes or answers every finding
#         -> it finds nothing on the exact head, CI green
#         -> (one-way paths, no CI: a Claude job judges it)
#         -> `review-settled` status -> merged at that head -> branch deleted
#            (a repo with `train` on: into its merge train instead, which
#            lands it once CI passes it merged onto trunk with the PRs ahead
#            of it; autoland/train.py)
#
# A draft is work in progress: nothing happens to it until it's ready. The
# owner only sees what needs them, in one issue: what a judge job couldn't
# decide, outside contributions. What's stuck goes to a Claude job, through
# proj's inbox. It also keeps every
# repo's settings at the baseline (docs/repo-baseline.md), new repos included.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.autoland;
  inherit (config.burrow) user stateDir;
  inherit (config.burrow.github) machineApp;
  account = config.users.users.${user};
  inherit (lib) mkOption types;
  rv = cfg.reviewers;

  # The review agents, and the user they run as: it has no GitHub login, and
  # reads only the snapshots the steward takes (secrets paths left out).
  agents = import ./autoland/agents/package.nix { inherit pkgs; };
  reviewer = "autoland-review";
  reviewerHome = "${stateDir}/autoland-review";
  reviewDir = "${stateDir}/autoland/review";
  # What the reviewers' service runs commands with, and so what an agent
  # reads the snapshot with, and runs the repo's own checks with (python3:
  # the scripts a repo checks itself with).
  reviewTools = with pkgs; [ bash config.programs.git.package bubblewrap coreutils findutils gnugrep gnused diffutils ripgrep python3 ];
  # The machine's own /bin/sh, /bin/bash and /usr/bin/env, as an agent's
  # sandbox shows them; a machine without /bin/sh or /usr/bin/env gives the
  # sandbox none either.
  reviewLinks = {
    "/bin/bash" = "${pkgs.bashInteractive}/bin/bash";
  } // lib.optionalAttrs (config.environment.binsh != null) {
    "/bin/sh" = config.environment.binsh;
  } // lib.optionalAttrs (config.environment.usrbinenv != null) {
    "/usr/bin/env" = config.environment.usrbinenv;
  };
  # All of the store an agent's sandbox shows: the agents, the bridge to
  # the gate, those tools, the links and the machine's CA bundle, with what
  # they need. Never the whole store, which holds every flake's sources this
  # machine has read, private ones included; nor /etc, whose files link into
  # the machine's own configuration.
  reviewStore = pkgs.closureInfo {
    rootPaths = [ agents stewardSrc config.security.pki.caBundle ]
      ++ reviewTools ++ lib.attrValues reviewLinks;
  };
  codexLogin = pkgs.writeShellScriptBin "autoland-codex-login" ''
    # Codex reviews on the ChatGPT plan's login: a device code to approve in
    # a browser. Run again whenever the review digest says it's logged out.
    exec /run/wrappers/bin/sudo -u ${reviewer} ${pkgs.coreutils}/bin/env HOME=${reviewerHome} CODEX_HOME=${reviewerHome}/.codex \
      ${agents}/bin/codex login --device-auth "$@"
  '';

  # The baseline and the house style it links to (the rules, the project
  # template and lib), together, so the links resolve for the job that reads
  # them.
  baselineDocs = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [ ../docs ../house ../templates ../lib ];
  };

  # The email Renovate commits as, when it has one of its own: any other
  # author on a Renovate branch is a Claude fix (steward.py). Without one,
  # Renovate commits as the user, like the fixes, and nothing tells them apart.
  renovateAuthor = config.burrow.updates.renovate.gitAuthor or null;
  renovateMatch = if renovateAuthor == null then null else builtins.match ".*<([^>]*)>.*" renovateAuthor;

  # renovate-sweep runs here: its verdicts on majors count, and its pass is
  # what a stalled wait on it kicks.
  sweeping = (config.burrow.updates.enable or false) && (config.burrow.updates.renovate.enable or false);
  watchdog = config.burrow.watchdog.enable or false;
  ledger = config.burrow.ledger;
  # The repos whose PRs land through their merge train.
  trains = lib.attrNames (lib.filterAttrs (_: r: r.train or false) cfg.repos);

  # The title Renovate gives each repo's dashboard (its
  # dependencyDashboardTitle default): the issue dashboard.py reads, and the
  # one whose events start a pass.
  dashboardTitle = "Dependency Dashboard";
  # The flaky tool, which lists each repo's quarantined tests: the issues
  # titled `flaky: TEST` (flakes.py, docs/flaky-tests.md).
  flaky = pkgs.callPackage ../tools/flaky/package.nix { };

  settings = {
    renovateEmail = if renovateMatch == null then null else lib.head renovateMatch;
    inherit (cfg) owners exclude context rulesets keepBranches baselineJobs baselinePerPass;
    app = { inherit (cfg.app) id slug; };
    inherit (cfg) authors;
    # The bots whose PRs and comments are the steward's authors' too, by
    # their Apps' slugs: GraphQL's logins for them, REST's without "[bot]".
    # The machinery's App posts and pushes the steward's Claude jobs' work.
    bots = [ "dependabot" "renovate" ] ++ lib.optional (machineApp != null) machineApp.slug;
    machinery = if machineApp == null then null else machineApp.name;
    stateDir = "${stateDir}/autoland";
    # The key renovate-sweep seals its verdicts on majors with (seal.py),
    # when it runs here; without it, none counts.
    majorKey = if sweeping then "${stateDir}/renovate-sweep/verdict.key" else null;
    # The longest a Renovate run goes on, when it runs here: how long after a
    # refresh request a run may still act on it, which the steward's jobs and
    # the watchdog's deadline have to outlast.
    renovateRun = if sweeping then config.systemd.services.renovate.serviceConfig.TimeoutStartSec else null;
    projectsDir = config.burrow.projectsDir;
    proj = "${config.burrow.tools.proj}/bin/proj";
    jobTimeout = cfg.jobTimeoutMinutes;
    jobIdle = cfg.jobIdleMinutes;
    owner = config.burrow.owner;
    cleanBranches = cfg.cleanBranches;
    labels = { inherit (cfg.labels) hold owner first; };
    notify = {
      inherit (config.burrow.notify) repo assignee;
      label = config.burrow.notify.label;
    };
    # What the pipeline is stuck on goes to proj's inbox, as an issue in
    # notify.repo with its label, for a Claude job to take (repos.py publish),
    # when the inbox will: notify.repo cloned in projectsDir, and the
    # steward's App one of inbox.authors (it is: see below). Otherwise it's
    # the owner's.
    inbox = { inherit (config.burrow.proj.inbox) enable label authors; };
    # Renovate's dashboards (dashboard.py), and where a job that ticks a box
    # on one kicks Renovate to read it.
    dashboard = {
      inherit (cfg.dashboard) enable waitDays errorDays;
      title = dashboardTitle;
      renovateKick = if config.burrow.updates.renovate.enable or false then "${config.burrow.events.kick}/renovate" else null;
      # Whose dashboards they are: Renovate runs as the machine's own App,
      # else as the user (null: one of `authors`).
      renovate = if machineApp == null then null else "${machineApp.slug}[bot]";
      # When Renovate's runs ended (updates/renovate.sh's RUNS), for an
      # error's two runs.
      renovateRuns = if config.burrow.updates.renovate.enable or false then "${config.burrow.updates.renovate.dir}/runs" else null;
    };
    # Quarantined flaky tests, each to a Claude job (flakes.py).
    flaky = {
      inherit (cfg.flaky) enable;
      bin = "${flaky}/bin/flaky";
    };
    # What every open PR waits on, for the stall watchdog (autoland/waits.py),
    # and what it took past a Claude job, for the stuck issue.
    watch = if watchdog then {
      kick = "${config.burrow.events.kick}/watchdog";
      escalated = "${config.burrow.watchdog.dir}/escalated.json";
      renovateSweep = sweeping;
    } else null;
    defaults = {
      inherit (cfg) rounds oneWay;
      handoffAfter = cfg.handoffAfterMinutes;
      sessionHandoffAfter = cfg.sessionHandoffAfterMinutes;
      settleDelay = cfg.settleDelayMinutes;
      stallAfter = cfg.stallAfterMinutes;
      holdStallAfter = cfg.holdStallAfterMinutes;
      renovateMergeAfter = cfg.renovateMergeAfterMinutes;
      directPush = false;
      ruleset = true;
      requireCi = true;
      proofJobs = { };
      proofBuilds = { };
      codex = true;
      skipPaths = [ ];
      train = false;
      trainSize = 8;
      trainRequire = [ ];
      trainStallAfter = cfg.trainStallAfterMinutes;
    };
    # The merge train's status, which a repo's baseline requires while its
    # train is on and gives up once it's off, the ledger on or not.
    trainContext = ledger.trainContext;
    # The merge train's `cn` (autoland/train.py), on the ledger's own log:
    # the train's ops are ledger ops. Its token command is the steward's own.
    train = if ledger.enable then {
      cn = "${ledger.package}/bin/cn";
      inherit (ledger) env;
      # Its candidates' branches, cn's to make and delete.
      branches = "conary/*";
    } else null;
    repos = cfg.repos;
    inherit (cfg) pipelineRepos leadRepos;
    # Every repo's hook to burrow-events (repos.py), with what each of its
    # handlers takes.
    webhook = {
      inherit (config.burrow.events) url;
      events = config.burrow.events.hookEvents;
    };
    # For the reviewers' answers coming back (review_worker.py).
    kick = "${config.burrow.events.kick}/autoland";
    reviewers = {
      codex = { inherit (rv.codex) enable effort parallel; };
    };
    inherit (rv) skipPaths;
    reviewTimeout = rv.timeoutMinutes;
    agents = "${agents}/bin";
    sandbox = "${pkgs.bubblewrap}/bin/bwrap";
    # What a proof would build, and whether it's built (proofs.py).
    nix = "${config.nix.package}/bin";
    box = {
      store = "${reviewStore}/store-paths";
      caBundle = config.security.pki.caBundle;
      links = reviewLinks;
    };
    baselineTask = ''
      REPO is new, and doesn't meet the baseline every repo of ${config.burrow.owner}'s keeps yet. Read
      ${baselineDocs}/docs/repo-baseline.md and bring this repo up to its "Files" section: open one PR, ready for review (not a draft), that adds
      what's missing (CI that builds and runs the tests through the repo's flake, AGENTS.md with the house blocks and the shared
      "Code Review Rules" section, CLAUDE.md, README, .gitignore, LICENSE if it's public) and changes
      nothing else. Don't ask for a review or merge it: the steward does that.
    '';
  };

  configFile = pkgs.writeText "autoland.json" (builtins.toJSON settings);
  # steward.py and every module beside it, linted as they're built: all of
  # them, so a new one can't be left behind.
  stewardSrc = pkgs.runCommand "autoland-steward-src"
    { nativeBuildInputs = [ pkgs.python3Packages.flake8 ]; } ''
    mkdir $out
    cp ${./autoland}/*.py $out/
    flake8 --ignore E501,W503 $out
  '';
  # Each of them imported from the package, with this machine's settings, as
  # part of the system's build: an import that doesn't resolve there (a
  # module the package lacks) fails the build, not every pass once it's live.
  # review_worker is imported only by the reviews' runs, so each one is.
  stewardImports = pkgs.runCommand "autoland-steward-imports" { } ''
    cd ${stewardSrc}
    AUTOLAND_CONFIG=${configFile} ${pkgs.python3}/bin/python3 -I -c '
    import importlib, pathlib, sys
    sys.path.insert(0, ".")
    for p in sorted(pathlib.Path(".").glob("*.py")):
        importlib.import_module(p.stem)
    '
    touch $out
  '';
  # What a pass runs: openssl signs the App's JWT (common.py), and the rest
  # talk to GitHub, unpack its tarballs and start Claude jobs. gh and git are
  # the machine's (github.nix): a pass's own calls pass the steward's token
  # in GH_TOKEN, which they use as given, and the rest (`status`, a
  # project's clone) go as the caller, its App's socket in a job's env too.
  stewardTools = [ (config.burrow.tools.gh or pkgs.gh) config.programs.git.package pkgs.gnutar pkgs.coreutils pkgs.openssl config.burrow.tools.proj ];
  # The command, with this machine's config, so `autoland-steward status`
  # works from any shell, not only in the service. A pass gets its tools
  # however it's started: by its units, or by hand or a Claude job
  # (`autoland-steward kick OWNER/REPO`) with a PATH of its own. Not a
  # review: its units give the reviewers only what they read with
  # (reviewTools).
  steward = pkgs.writeShellScriptBin "autoland-steward" ''
    export AUTOLAND_CONFIG=''${AUTOLAND_CONFIG:-${configFile}}
    steward_path=${lib.makeBinPath stewardTools}
    [ "''${1:-}" = review ] || export PATH="$steward_path''${PATH:+:$PATH}"
    exec ${pkgs.python3}/bin/python3 -E -s ${stewardSrc}/steward.py "$@"
  '';
in
{
  imports = [
    (import ./common.nix inputs)
    { key = "burrow/notify"; imports = [ (import ./notify.nix inputs) ]; }
    { key = "burrow/proj"; imports = [ (import ./proj.nix inputs) ]; }
    { key = "burrow/events"; imports = [ (import ./events.nix inputs) ]; }
    { key = "burrow/github"; imports = [ (import ./github.nix inputs) ]; }
    # The train's: its status's name, whether the ledger is on or not.
    { key = "burrow/ledger"; imports = [ (import ./ledger.nix inputs) ]; }
    { key = "burrow/watchdog/sources"; imports = [ ./watchdog/sources.nix ]; }
  ];

  options.burrow.autoland = {
    enable = lib.mkEnableOption "autoland, the PR steward for every repo you own";

    owners = mkOption {
      type = types.listOf types.str;
      example = [ "sam" "sams-org" ];
      description = "GitHub users and orgs whose repos it covers (every non-archived, non-fork repo, new ones included).";
    };

    app = {
      id = mkOption {
        type = types.ints.positive;
        example = 123456;
        description = ''
          The ID of the GitHub App the steward acts as, installed on every
          one of `owners` for all their repos. It needs read and write on
          administration, contents, issues, pull requests, commit statuses,
          webhooks and workflows, and read on checks and actions
          (docs/repo-baseline.md). Everything the steward does on GitHub,
          it does as the App, never as the machine user's own login, which
          its Claude jobs and every Claude session share: so only the App
          can post its `context` status or its reviewers' reviews.
        '';
      };
      slug = mkOption {
        type = types.strMatching "[a-z0-9][a-z0-9-]*";
        example = "sams-steward";
        description = "The App's slug: it posts as SLUG[bot].";
      };
      keyFile = mkOption {
        type = types.str;
        example = "/run/secrets/autoland-app-key";
        description = ''
          The App's private key (PEM), in a file only root can read, outside
          the store: a sops secret, say. Each pass is given it as a systemd
          credential, and mints the App's tokens from it, in memory only. No
          sandbox, and no Claude elsewhere holding the owner's GitHub token,
          can act as the App; a Claude on this machine running as
          burrow.user, with its sudo, still can.
        '';
      };
    };

    authors = mkOption {
      type = types.listOf types.str;
      example = [ "sam" ];
      description = ''
        Whose PRs merge themselves once settled: you, and so your Claude
        sessions, which push as you. Dependabot's and Renovate's Apps are
        added, and the machinery's (burrow.github.machinery), which the
        steward's own Claude jobs act as. Anyone else's PR gets reviewed the
        same way and then waits for you.
      '';
    };

    exclude = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "sam/scratch-*" ];
      description = "Repos to leave alone (OWNER/NAME globs).";
    };

    pipelineRepos = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "sam/burrow" "sam/box" ];
      description = ''
        The repos whose PRs work on this pipeline itself (OWNER/NAMEs, any
        case): the steward, its reviewers, the machine they run on. Their
        reviews take a reviewer's next free place before anyone else's, and
        the Claude jobs in their projects that the steward and proj-inbox
        start (`proj run --pipeline`) go ahead of everything, in proj's
        `pipelineSlots`, so the work queued behind them gets the speedup
        sooner. The steward's jobs in any other repo go ahead of the line as
        well, but within that project's `maxJobsPerProject`, so one repo's
        PRs can't fill the machine.
      '';
    };

    leadRepos = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "sam/burrow" ];
      description = ''
        The repos the pipeline itself is built from (OWNER/NAMEs, any case),
        whose work goes first of all: ahead of everyone else's, the rest of
        `pipelineRepos` included. Their reviews take a reviewer's next free
        place first, and proj puts their pipeline jobs (the steward's Claude
        jobs, proj-inbox's issues) first in its line. A lead repo is
        pipeline work whether or not `pipelineRepos` lists it.
      '';
    };

    dryRun = mkOption {
      type = types.bool;
      default = false;
      description = ''
        Only log what each pass would do (`autoland-steward status`), changing
        nothing on GitHub: for a first look at a new machine's repos.
      '';
    };

    rounds = mkOption {
      type = types.ints.positive;
      default = 5;
      description = "Review rounds with findings (one reviewer's, whichever has had more) before its reviewers stop looking: a Claude job then fixes or answers what's left, and a judge decides whether it lands.";
    };

    oneWay = mkOption {
      type = types.listOf types.str;
      default = [ "(^|/)CODEOWNERS$" "(^|/)secrets?/" "(^|/)\\.sops\\.yaml$" "\\.sops\\.(ya?ml|json|env)$" ];
      description = ''
        Path regexes that make a settled PR one a Claude job judges before it
        lands: it lands anything a revert or a follow-up can undo, and sends
        you only what can't be undone or is yours to decide. The default is
        CODEOWNERS (who signs off on what) and secrets. The rest of
        `.github/` isn't on it: the reviewers read workflow changes like any
        other, and the steward merges them once settled. Nor are database
        migrations: what undoes a bad one is the backup its deploy takes
        before migrating, not a wait at merge. A repo whose deploy takes no
        such backup can put `(^|/)migrations?/` on its own list (`repos`).
      '';
    };

    handoffAfterMinutes = mkOption {
      type = types.ints.positive;
      default = 10;
      description = ''
        How long open findings or red CI sit with nobody pushing or replying
        before a Claude job on this machine takes them.
      '';
    };

    sessionHandoffAfterMinutes = mkOption {
      type = types.ints.positive;
      default = 30;
      description = ''
        The same wait for a PR whose body links the Claude session that
        opened it (`https://claude.ai/code/session_…`). That session hears of
        every finding and red check and fixes them itself, so a job on this
        machine only takes over once it has been quiet this long, in case
        it's gone; sooner, the two push over each other.
      '';
    };

    settleDelayMinutes = mkOption {
      type = types.ints.positive;
      default = 1;
      description = "How long a new head sits still before the reviewers are asked, so a burst of pushes gets one review.";
    };

    stallAfterMinutes = mkOption {
      type = types.ints.positive;
      default = 120;
      description = ''
        How long a PR may wait on one thing (its status's description: a
        review, CI, a Claude job, a merge) before the stall watchdog, when
        it's on, takes it: kicks its pass, then hands it to a Claude job.
      '';
    };

    trainStallAfterMinutes = mkOption {
      type = types.ints.positive;
      default = 240;
      description = ''
        The same for a PR in its repo's merge train, while one candidate is
        in flight (its own, or the one ahead of it): long enough for that
        candidate's CI, a second try of a test that failed, and its PRs'
        merges.
      '';
    };

    holdStallAfterMinutes = mkOption {
      type = types.ints.positive;
      default = 480;
      description = ''
        The same for a PR on hold: a Claude job then finds out what it waits
        on, and moves one that waits on the owner's word to the owner label.
      '';
    };

    renovateMergeAfterMinutes = mkOption {
      type = types.ints.positive;
      default = 60;
      description = ''
        How long a Renovate PR with automerge on may sit settled and green
        before the stall watchdog takes it: a Renovate run on its repo, which
        merges it, then a Claude job.
      '';
    };

    jobTimeoutMinutes = mkOption {
      type = types.ints.positive;
      default = 60;
      description = ''
        How long each turn of a Claude job the steward starts (a fix, a
        judge, a proof run) may run before proj stops it. Its end, however
        it ends, starts the repo's pass, and the steward books one for this
        deadline too in case that doesn't get through: a job that ended
        without its answer is started once more, and a second is a failed
        status on the PR.
      '';
    };

    jobIdleMinutes = mkOption {
      type = types.ints.positive;
      default = 20;
      description = ''
        A turn of one of those jobs whose stream has said nothing this long
        is stuck, and proj stops it then, however long its work may take: one
        at work says something at least each time Claude's Bash tool stops a
        command, ten minutes at most. It ends as one past jobTimeout does.
      '';
    };

    context = mkOption {
      type = types.str;
      default = "review-settled";
      description = "The commit status it posts, and that the baseline ruleset requires.";
    };

    rulesets = mkOption {
      type = types.bool;
      default = true;
      description = ''
        Give every repo whose plan allows it a `baseline` ruleset on its default
        branch: no force pushes or deleting it, PRs only, review threads
        resolved, the status required, CODEOWNERS honoured, and repo admins
        (you) can merge past it. Only added when missing; never edited.
      '';
    };

    cleanBranches = mkOption {
      type = types.bool;
      default = true;
      description = ''
        Daily, delete branches whose tip is already in the default branch or
        is a closed PR's head (no open PR). Other unmerged ones with no commit
        for 3 days (not a bot's) are kept at refs/archive/BRANCH/SHA, then
        deleted (docs/repo-baseline.md says how to restore one).
      '';
    };

    dashboard = {
      enable = mkOption {
        type = types.bool;
        default = (config.burrow.updates.enable or false) && (config.burrow.updates.renovate.enable or false);
        defaultText = lib.literalExpression "burrow.updates.renovate.enable";
        description = ''
          Watch each repo's Renovate Dependency Dashboard (autoland/dashboard.py):
          an update that sits on it too long without a PR goes to a Claude job,
          and one still there a day after that job to the stuck issue. Its
          job reads Renovate's journal, so it wants Renovate on this machine.
        '';
      };
      waitDays = mkOption {
        type = types.ints.positive;
        default = 4;
        description = ''
          How long an update waiting on something (a release's soak, its
          schedule, a rate limit, an approval) sits on the dashboard before
          it's stuck. Keep it a day past the soak Renovate gives a release
          (minimumReleaseAge, burrow.updates), or every soak is stuck.
        '';
      };
      errorDays = mkOption {
        type = types.ints.positive;
        default = 1;
        description = ''
          How long an error, a problem with the repo or a failed lookup sits on
          the dashboard before it's stuck, at most: with Renovate on this
          machine, one still there after two of Renovate's runs on the repo
          is stuck then.
        '';
      };
    };

    flaky.enable = mkOption {
      type = types.bool;
      default = true;
      description = ''
        Send each repo's quarantined flaky tests (open issues titled `flaky:
        TEST`, labelled `flaky`, that its gate files: docs/flaky-tests.md) to a
        Claude job that fixes them, and one still open a day after that job to
        the stuck issue (autoland/flakes.py).
      '';
    };

    keepBranches = mkOption {
      type = types.listOf types.str;
      default = [ "gh-pages" "release/*" "releases/*" ];
      description = "Branch globs the cleanup never touches.";
    };

    baselineJobs = mkOption {
      type = types.bool;
      default = true;
      description = ''
        Every repo gets one Claude job (a few per pass, the first time it's seen,
        existing repos included) that opens a PR bringing its files up to
        the baseline, if they aren't already.
      '';
    };

    baselinePerPass = mkOption {
      type = types.ints.positive;
      default = 2;
      description = "At most this many baseline jobs start in one pass (the first pass would otherwise start one per repo).";
    };

    labels = {
      hold = mkOption {
        type = types.str;
        default = "hold";
        description = ''
          A PR with this label is paused: its status goes back to pending and
          nothing merges it. For a wait on something else (another PR, a
          deploy); one that waits for you takes the owner label, which lists
          it for you.
        '';
      };
      owner = mkOption {
        type = types.str;
        default = "needs-owner";
        description = "Marks a PR that waits for you; take it off to let the steward carry on.";
      };
      first = mkOption {
        type = types.str;
        default = "first";
        description = ''
          A PR with this label goes ahead of every PR without it in its repo's
          line for proofSlots, and the oldest first among those with it: for a
          fix the rest of the line waits on, opened after them.
        '';
      };
    };

    reviewers = {
      codex = {
        enable = lib.mkEnableOption ''
          Codex as a reviewer: the codex CLI, logged in to your ChatGPT plan
          (`autoland-codex-login`, once). Its model is codex's default, unless
          the reviewers' home has a `.codex/config.toml` that names one: which
          model does what is the machine's business. It's the steward's one
          reviewer: while the plan's usage is spent, or it's logged out, PRs
          wait for it, and it comes back on its own
        '';
        effort = mkOption {
          type = types.enum [ "low" "medium" "high" "xhigh" ];
          default = "high";
          description = "How hard it thinks (`model_reasoning_effort`).";
        };
        parallel = mkOption {
          type = types.ints.positive;
          default = 2;
          description = "At most this many Codex reviews at once.";
        };
      };

      timeoutMinutes = mkOption {
        type = types.ints.positive;
        default = 30;
        description = ''
          How long one review may run before it's stopped and counted a
          failed try, like any other error: the steward asks again, three
          tries in all, then three more an hour on.
        '';
      };

      skipPaths = mkOption {
        type = types.listOf types.str;
        default = [ "(^|/)secrets?/" "(^|/)\\.sops\\.yaml$" "\\.sops\\.(ya?ml|json|env)$" "(^|/)\\.env(\\..*)?$" "\\.(pem|key|p12|pfx)$" ];
        description = ''
          Path regexes left out of the snapshot the reviewers read, on both
          sides of the diff: they're told such a file changed, not how, so a
          PR that changes one is judged before it lands. A directory matches
          with a trailing slash. A repo's own `skipPaths` (in `repos`) adds
          to these.
        '';
      };
    };

    repos = mkOption {
      type = types.attrsOf types.attrs;
      default = { };
      example = lib.literalExpression ''
        {
          # Its update pipeline pushes to main: rules stop force pushes and deletion only.
          "sam/homelab".directPush = true;
          "sam/website" = { oneWay = [ "^deploy/" ]; rounds = 3; };
          "sam/firmware".proofJobs.hil = "Run `just hil` on the bench box and post its result as the `hil` commit status.";
        }
      '';
      description = ''
        Per-repo overrides (OWNER/NAME): any of rounds, oneWay,
        handoffAfter, sessionHandoffAfter, settleDelay, stallAfter,
        holdStallAfter, renovateMergeAfter (minutes), directPush,
        ruleset (false: no baseline ruleset), requireCi (false: a PR with no
        checks at all counts as green, for a repo with nothing to test; by
        default a Claude job judges a settled PR in a repo with no CI,
        building and testing it itself), proofJobs (checks that run on a
        machine rather than in CI, `{ "<check>" = "<how to run it and record
        its verdict>"; }`: once a settled PR waits on nothing else, one
        Claude job a head runs them on that commit and records each verdict,
        a failure as a failure), proofSlots (how many of those jobs the repo
        may have building at once, for proofs that take the whole machine:
        the rest wait their turn, those labelled `labels.first` first, then
        the oldest PR first; unset, no limit),
        proofBuilds (the flake attribute each proof check builds, `{
        "<check>" = "checks.x86_64-linux.<name>"; }`, whose result is the
        check's verdict: a proof whose attributes' outputs are all in the
        store takes no job, its verdicts taken from the store, saying so
        and naming each derivation; one whose outputs are being built by a
        proof already running skips the line and holds no slot; a job
        running a check without one holds a slot of its own), keepBranches
        (more branch globs the cleanup never touches in this repo, on top of
        the machine's, for branches a repo keeps as evidence), codex (false:
        Codex leaves this repo alone, and none of it goes to it), skipPaths (more path regexes the reviewers never read in
        this repo, on top of `reviewers.skipPaths`, never instead of them: what
        the repo keeps private that isn't a secret anywhere else), train
        (true: a PR it settles goes into the repo's merge train, Conary's
        `cn train`, instead of being merged: the train merges the PRs in it
        onto the default branch together, has CI run on that on a
        `conary/train/` branch, lands them in order once it's green, and
        splits a red one to find the PR that broke it. A PR touching a
        oneWay path rides alone, landing only while the default branch is
        still the base its CI ran on. CI on the PRs themselves is optional:
        with none, a reviewed PR goes straight in. It needs burrow.ledger,
        isn't directPush, and has no proofJobs yet), trainSize (the
        most PRs a candidate holds, 8), trainRequire (checks the train's CI
        waits for even before they report, when more than one workflow
        runs there), trainStallAfter (minutes, trainStallAfterMinutes).
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    system.checks = [ stewardImports ];
    assertions = [{
      assertion = rv.codex.enable;
      message = "burrow.autoland needs its reviewer: turn on burrow.autoland.reviewers.codex.";
    } {
      # It keeps every repo's hook, which GitHub's deliveries come in on.
      assertion = config.burrow.events.host != null;
      message = "burrow.autoland needs burrow.events.host and .secretFile: GitHub's events reach its passes through them.";
    } {
      assertion = trains == [ ] || ledger.enable;
      message = "burrow.autoland: a repo's train (${lib.concatStringsSep ", " trains}) is on the ledger's log: turn on burrow.ledger.";
    } {
      # A proof runs on a PR's head; the train's evidence is its candidates'.
      assertion = lib.all (r: (cfg.repos.${r}.proofJobs or { }) == { }) trains;
      message = "burrow.autoland: a repo with its train on can't have proofJobs yet.";
    } {
      # Its rules are what keep anything but the train off its default branch.
      assertion = trains == [ ] || cfg.rulesets && lib.all (r: !(cfg.repos.${r}.directPush or false) && (cfg.repos.${r}.ruleset or true)) trains;
      message = "burrow.autoland: a repo with its train on needs its baseline ruleset (rulesets, and the repo's ruleset) and can't be directPush: only the train may move its default branch.";
    }];

    burrow.proj.enable = true;
    burrow.notify.enable = true;
    # What the pipeline is stuck on goes to the inbox as an issue the App
    # opens (repos.py publish).
    burrow.proj.inbox.authors = [ "${cfg.app.slug}[bot]" ];

    environment.systemPackages = [ steward ] ++ lib.optional rv.codex.enable codexLogin;

    users.users.${reviewer} = {
      isSystemUser = true;
      group = reviewer;
      home = reviewerHome;
      createHome = true;
    };
    users.groups.${reviewer} = { };

    # Every PR's pass comes from what happens to it (burrow.events): a push, a
    # review, a comment or CI finishing on it starts its repo's pass
    # (autoland@OWNER-REPO), and so do a Claude job's turn ending and a
    # reviewer's answer coming back. A pass that waits on time (a new head's
    # settle delay, a reviewer back in service) books its repo's next one in
    # wake.json.
    # The full pass (settings, new repos, branches) is burrow-events'
    # catch-up, when it starts and hourly.
    burrow.events.handlers.autoland = {
      # The PR itself (opened, ready, labels, retargeted), pushes, reviews and
      # their threads, comments (a judge's verdict), CI finishing, and commit
      # statuses (a proof's verdict).
      events = [ "push" "pull_request" "pull_request_review" "pull_request_review_comment"
        "pull_request_review_thread" "issue_comment" "check_suite" "status" ]
        ++ lib.optional (cfg.dashboard.enable || cfg.flaky.enable) "issues";
      # Of the issues, only Renovate's dashboard (dashboard.py) and flaky
      # tests' (flakes.py; the flaky tool's test.rs has the title).
      issueTitles = lib.optional cfg.dashboard.enable dashboardTitle
        ++ lib.optional cfg.flaky.enable "flaky: *";
      unit = "autoland";
      wake = "${stateDir}/autoland/wake.json";
      # Its own status tells it nothing (a job awaiting the PR does want it).
      ownStatuses = [ cfg.context ];
    };
    systemd.services = lib.genAttrs [ "autoland" "autoland@" ] (name: {
      description = if name == "autoland"
        then "autoland: settle and merge every PR, keep every repo at the baseline"
        else "autoland: settle and merge the PRs of %I";
      # A switch leaves a running one be (see burrow.switch's check).
      restartIfChanged = false;
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      path = stewardTools;
      serviceConfig = {
        Type = "oneshot";
        User = user;
        ExecStart = "${steward}/bin/autoland-steward"
          + (if cfg.dryRun then " status" else lib.optionalString (name == "autoland@") " kick %I");
        TimeoutStartSec = "20min";
        Nice = 10;
        # The repos' hooks' secret (repos.py), and the App's key.
        LoadCredential = [ "webhook:${config.burrow.events.secretFile}" "app-key:${cfg.app.keyFile}" ];
      };
    }) // {
      # The reviews the passes queue, as soon as there are any, as the
      # reviewers' own user: no GitHub login, and the snapshots are all it
      # can read of the repos.
      autoland-review = lib.mkIf (!cfg.dryRun) {
        description = "autoland: the reviewers' reviews of the queued PR heads";
        # A switch leaves a running one be (see burrow.switch's check).
        restartIfChanged = false;
        after = [ "network-online.target" ];
        wants = [ "network-online.target" ];
        # What the agents read the snapshot with, and bubblewrap for codex's
        # own sandbox inside theirs.
        path = reviewTools;
        environment.HOME = reviewerHome;
        # The gate's TLS to the agents' APIs (autoland/egress.py).
        environment.SSL_CERT_FILE = config.security.pki.caBundle;
        serviceConfig = {
          Type = "oneshot";
          User = reviewer;
          ExecStart = "${steward}/bin/autoland-steward review";
          # However long an agent thinks: no pass gives up on a queued review either.
          TimeoutStartSec = "infinity";
          Nice = 10;
          ProtectSystem = "strict";
          ProtectHome = true;
          PrivateTmp = true;
          ReadWritePaths = [ reviewerHome "${reviewDir}/queue" "${reviewDir}/done" "${reviewDir}/sessions" ];
        };
      };
    };
    systemd.paths.autoland-review = lib.mkIf (!cfg.dryRun) {
      wantedBy = [ "paths.target" ];
      # A review to do, or a closed PR's sessions to delete.
      pathConfig = {
        DirectoryNotEmpty = "${reviewDir}/queue";
        PathExistsGlob = "${reviewDir}/sessions/.gone-*";
      };
    };

    # The steward's state, and the reviews: it queues them and takes the
    # snapshots; the reviewers' user reads those, answers in done/, takes
    # what it answered off the queue and keeps each open PR's sessions in
    # sessions/, which the
    # steward marks gone when the PR closes. The clones the snapshots come
    # from are the steward's alone.
    systemd.tmpfiles.settings.burrow = let
      dir = mode: group: { d = { inherit mode user group; }; };
    in {
      "${stateDir}/autoland" = dir "0755" account.group;
      ${reviewDir} = dir "2750" reviewer;
      "${reviewDir}/queue" = dir "2770" reviewer;
      "${reviewDir}/done" = dir "2770" reviewer;
      "${reviewDir}/sessions" = dir "2770" reviewer;
      "${reviewDir}/snap" = dir "2750" reviewer;
      "${reviewDir}/git" = dir "0700" account.group;
    };

    burrow.watchdog.sources.autoland = {
      file = "${settings.stateDir}/watch.json";
      about = "The PR steward (autoland)";
      look = "Its status on the PR says what it waits on; `autoland-steward status`, `journalctl -u autoland` and its state in ${settings.stateDir} say why.";
      kick = "autoland";
      # It writes on every pass, and has a full one at least hourly.
      staleMinutes = 120;
    };

    burrow.claude.machineMd = lib.mkOrder 1050 ''
      ## Pull requests

      Every repo of ${lib.concatStringsSep ", " cfg.owners} merges its PRs on its own, through the
      steward (`autoland`, on every change to a PR). Open PRs ready for review, never as drafts (a
      draft is left alone until it's ready), then leave them be:

      - Codex reviews every head; the steward posts its reviews as its GitHub App,
        ${cfg.app.slug}[bot], each thread marked as Codex's. Fix or answer
        every finding (reply on the thread, push the fix) and resolve the thread; Codex
        looks again once they're all resolved.
      - Once it finds nothing on the exact head, every thread is resolved and CI is
        green, it posts `${cfg.context}` and merges it, `.github/` changes included; the
        branch goes too.${lib.optionalString (trains != [ ]) ''
          In ${lib.concatStringsSep ", " trains} it goes into the repo's merge train
          instead, which lands it once CI is green on it merged onto the default branch with
          the PRs ahead of it (`cn train show OWNER/REPO` says where it is). A `${ledger.trainContext}`
          failure on its head means it broke that merge alone: a Claude job fixes it.''}
      - A settled PR that touches a one-way path (secrets, CODEOWNERS), or
        a repo with no CI, gets a Claude job that judges it: it lands what a revert or a
        follow-up can undo, and sends ${config.burrow.owner} only what can't be undone or is theirs to decide.
        The judge doesn't merge: the steward lands the PR on its verdict. Outside a
        steward job, a Claude session may merge such a PR itself (squash, delete the
        branch) once it has made that call.
      - ${config.burrow.owner}'s standing authority: merges, GitHub settings changes and cleanup
        are pre-approved. Take the judgment call first; only a genuine one-way door
        you can't decide waits for them, labelled `${cfg.labels.owner}` and listed in one issue
        (so are outside contributions). A PR past ${toString cfg.rounds} review rounds isn't theirs:
        its last findings are fixed or answered, and a judge decides.
        `${cfg.labels.hold}` keeps the steward off a PR while it waits on something else (another
        PR, a deploy); one waiting on ${config.burrow.owner}'s OK gets `${cfg.labels.owner}` instead, which
        lists it for them.${lib.optionalString watchdog " A hold still on after ${toString cfg.holdStallAfterMinutes} minutes gets a Claude job's look."}
      - `autoland-steward status` shows where every open PR stands, changing nothing.
      - Every repo, new ones too, is kept at the baseline in burrow's
        docs/repo-baseline.md.
    '';
  };
}
