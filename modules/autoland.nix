# autoland: every PR in every repo you own goes from ready to merged on its
# own, once a reviewer has nothing left to say (autoland/steward.py).
#
#   ready -> Codex and DeepSeek review it (agents reading a snapshot of the
#            head, as a user of their own with no GitHub access)
#         -> Claude fixes or answers every finding
#         -> both find nothing on the exact head, CI green
#         -> (one-way paths, no CI: a Claude job judges it)
#         -> `review-settled` status -> auto-merge -> branch deleted
#
# A draft is work in progress: nothing happens to it until it's ready. The
# owner only sees what needs them, in one issue: what a judge job couldn't
# decide, PRs past the round limit, outside contributions, anything stuck. It also keeps every
# repo's settings at the baseline (docs/repo-baseline.md), new repos included.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.autoland;
  inherit (config.burrow) user stateDir;
  account = config.users.users.${user};
  inherit (lib) mkOption types;
  rv = cfg.reviewers;

  # The review agents, and the user they run as: it has no GitHub login, and
  # reads only the snapshots the steward takes (secrets paths left out).
  agents = import ./autoland/agents/package.nix { inherit pkgs; };
  reviewer = "autoland-review";
  reviewerHome = "${stateDir}/autoland-review";
  reviewDir = "${stateDir}/autoland/review";
  codexLogin = pkgs.writeShellScriptBin "autoland-codex-login" ''
    # Codex reviews on the ChatGPT plan's login: a device code to approve in
    # a browser. Run again whenever the review digest says it's logged out.
    exec /run/wrappers/bin/sudo -u ${reviewer} env HOME=${reviewerHome} CODEX_HOME=${reviewerHome}/.codex \
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

  settings = {
    renovateEmail = if renovateMatch == null then null else lib.head renovateMatch;
    inherit (cfg) owners exclude context rulesets keepBranches baselineJobs baselinePerPass;
    # The bots' logins as GitHub's GraphQL API gives them (no "[bot]").
    authors = cfg.authors ++ [ "dependabot" "renovate" ];
    stateDir = "${stateDir}/autoland";
    projectsDir = config.burrow.projectsDir;
    proj = "${config.burrow.tools.proj}/bin/proj";
    owner = config.burrow.owner;
    cleanBranches = cfg.cleanBranches;
    labels = { inherit (cfg.labels) hold owner; };
    notify = {
      inherit (config.burrow.notify) repo assignee;
      label = config.burrow.notify.label;
    };
    defaults = {
      inherit (cfg) rounds oneWay;
      handoffAfter = cfg.handoffAfterMinutes;
      settleDelay = cfg.settleDelayMinutes;
      directPush = false;
      ruleset = true;
      requireCi = true;
      proofJobs = { };
      codex = true;
      deepseek = true;
    };
    repos = cfg.repos;
    reviewers = {
      codex = { inherit (rv.codex) enable effort parallel; };
      deepseek = { inherit (rv.deepseek) enable effort parallel; };
    };
    inherit (rv) skipPaths;
    agents = "${agents}/bin";
    sandbox = "${pkgs.bubblewrap}/bin/bwrap";
    baselineTask = ''
      REPO is new, and doesn't meet the baseline every repo of ${config.burrow.owner}'s keeps yet. Read
      ${baselineDocs}/docs/repo-baseline.md and bring this repo up to its "Files" section: open one PR, ready for review (not a draft), that adds
      what's missing (CI that builds and runs the tests through the repo's flake, AGENTS.md with the house block and the shared "Pull requests" and
      "Code Review Rules" sections, CLAUDE.md, README, .gitignore, LICENSE if it's public) and changes
      nothing else. Don't ask for a review or merge it: the steward does that.
    '';
  };

  reviewerNames = lib.concatStringsSep " and "
    (lib.optional rv.codex.enable "Codex" ++ lib.optional rv.deepseek.enable "DeepSeek");

  configFile = pkgs.writeText "autoland.json" (builtins.toJSON settings);
  # steward.py and the modules it imports from beside it, linted as they're
  # built.
  stewardSrc = pkgs.runCommand "autoland-steward-src"
    { nativeBuildInputs = [ pkgs.python3Packages.flake8 ]; } ''
    mkdir $out
    cp ${./autoland/steward.py} $out/steward.py
    cp ${./autoland/common.py} $out/common.py
    cp ${./autoland/reviewers.py} $out/reviewers.py
    cp ${./autoland/review_worker.py} $out/review_worker.py
    cp ${./autoland/repos.py} $out/repos.py
    cp ${./autoland/tasks.py} $out/tasks.py
    flake8 --ignore E501,W503 $out
  '';
  # The command, with this machine's config, so `autoland-steward status`
  # works from any shell, not only in the service.
  steward = pkgs.writeShellScriptBin "autoland-steward" ''
    export AUTOLAND_CONFIG=''${AUTOLAND_CONFIG:-${configFile}}
    exec ${pkgs.python3}/bin/python3 -E -s ${stewardSrc}/steward.py "$@"
  '';
in
{
  imports = [
    (import ./common.nix inputs)
    { key = "burrow/notify"; imports = [ (import ./notify.nix inputs) ]; }
    { key = "burrow/proj"; imports = [ (import ./proj.nix inputs) ]; }
  ];

  options.burrow.autoland = {
    enable = lib.mkEnableOption "autoland, the PR steward for every repo you own";

    owners = mkOption {
      type = types.listOf types.str;
      example = [ "sam" "sams-org" ];
      description = "GitHub users and orgs whose repos it covers (every non-archived, non-fork repo, new ones included).";
    };

    authors = mkOption {
      type = types.listOf types.str;
      example = [ "sam" ];
      description = ''
        Whose PRs merge themselves once settled: you, and so your Claude
        sessions, which push as you. Dependabot and Renovate's app are added. Anyone else's PR
        gets reviewed the same way and then waits for you.
      '';
    };

    exclude = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "sam/scratch-*" ];
      description = "Repos to leave alone (OWNER/NAME globs).";
    };

    dryRun = mkOption {
      type = types.bool;
      default = false;
      description = ''
        Only log what each pass would do (`autoland-steward status`), changing
        nothing on GitHub: for a first look at a new machine's repos.
      '';
    };

    every = mkOption {
      type = types.str;
      default = "5min";
      description = "Time between passes (after one ends).";
    };

    rounds = mkOption {
      type = types.ints.positive;
      default = 5;
      description = "Review rounds with findings (one reviewer's, whichever has had more) before it stops and asks you.";
    };

    oneWay = mkOption {
      type = types.listOf types.str;
      default = [ "(^|/)CODEOWNERS$" "(^|/)secrets?/" "(^|/)\\.sops\\.yaml$" "\\.sops\\.(ya?ml|json|env)$" "(^|/)migrations?/" ];
      description = ''
        Path regexes that make a settled PR one a Claude job judges before it
        lands: it lands anything a revert or a follow-up can undo, and sends
        you only what can't be undone or is yours to decide. The default is
        CODEOWNERS (who signs off on what), secrets and database migrations.
        The rest of `.github/` isn't on it: the reviewers read workflow
        changes like any other, and the steward merges them once settled.
      '';
    };

    handoffAfterMinutes = mkOption {
      type = types.ints.positive;
      default = 30;
      description = ''
        How long open findings or red CI sit with nobody pushing or replying
        before a Claude job on this machine takes them. Usually the session
        that opened the PR fixes them first.
      '';
    };

    settleDelayMinutes = mkOption {
      type = types.ints.positive;
      default = 3;
      description = "How long a new head sits still before the reviewers are asked, so a burst of pushes gets one review.";
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
        Daily, delete branches whose tip is already in the default branch (no
        open PR, not a bot's). Unmerged ones older than 30 days are listed for
        you, never deleted.
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
        description = "A PR with this label is paused: its status goes back to pending and nothing merges it.";
      };
      owner = mkOption {
        type = types.str;
        default = "needs-owner";
        description = "Marks a PR that waits for you; take it off to let the steward carry on.";
      };
    };

    reviewers = {
      codex = {
        enable = lib.mkEnableOption ''
          Codex as a reviewer: the codex CLI, logged in to your ChatGPT plan
          (`autoland-codex-login`, once). Its model is codex's default, unless
          the reviewers' home has a `.codex/config.toml` that names one: which
          model does what is the machine's business. While the plan's usage is
          spent, or it's logged out, PRs settle on the other reviewers, and it
          comes back on its own
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

      deepseek = {
        enable = lib.mkEnableOption ''
          DeepSeek as a reviewer: DeepSeek Harness (dsh), headless, on its API.
          Its model is dsh's default, unless the reviewers' home has a
          `.dsh/profiles/headless/cordis.patch.yml` that sets one (its
          `agent-default-model` entry, provider and model both). What it
          reads goes to DeepSeek, private repos included; `skipPaths` are
          left out
        '';
        envFile = mkOption {
          type = types.str;
          example = "/run/secrets/deepseek-review";
          description = ''
            A root-only file of KEY=value lines with DEEPSEEK_API_KEY. Only the
            reviewers' service reads it (systemd's LoadCredential), and the
            key is taken out of anything an agent says.
          '';
        };
        effort = mkOption {
          type = types.enum [ "low" "high" "max" ];
          default = "max";
          description = ''
            How hard it thinks. At max, a review that doesn't answer is asked
            once more at high: it can think past its budget (the API's
            ceiling, 384k tokens) without answering.
          '';
        };
        parallel = mkOption {
          type = types.ints.positive;
          default = 4;
          description = "At most this many DeepSeek reviews at once.";
        };
      };

      skipPaths = mkOption {
        type = types.listOf types.str;
        default = [ "(^|/)secrets?/" "(^|/)\\.sops\\.yaml$" "\\.sops\\.(ya?ml|json|env)$" "(^|/)\\.env(\\..*)?$" "\\.(pem|key|p12|pfx)$" ];
        description = ''
          Path regexes left out of the snapshot the reviewers read, on both
          sides of the diff: they're told such a file changed, not how. A
          directory matches with a trailing slash.
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
        handoffAfter, settleDelay (minutes), directPush,
        ruleset (false: no baseline ruleset), requireCi (false: a PR with no
        checks at all counts as green, for a repo with nothing to test; by
        default a Claude job judges a settled PR in a repo with no CI,
        building and testing it itself), proofJobs (checks that run on a
        machine rather than in CI, `{ "<check>" = "<how to run it and record
        its verdict>"; }`: once a settled PR waits on nothing else, one
        Claude job a head runs them on that commit and records each verdict,
        a failure as a failure), keepBranches
        (more branch globs the cleanup never touches in this repo, on top of
        the machine's, for branches a repo keeps as evidence), codex and
        deepseek (false: that reviewer leaves this repo alone, and none of it
        goes to it).
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [{
      assertion = rv.codex.enable || rv.deepseek.enable;
      message = "burrow.autoland needs a reviewer: turn on burrow.autoland.reviewers.codex or .deepseek.";
    }];

    burrow.proj.enable = true;
    burrow.notify.enable = true;

    environment.systemPackages = [ steward ] ++ lib.optional rv.codex.enable codexLogin;

    users.users.${reviewer} = {
      isSystemUser = true;
      group = reviewer;
      home = reviewerHome;
      createHome = true;
    };
    users.groups.${reviewer} = { };

    systemd.services.autoland = {
      description = "autoland: settle and merge every PR, keep every repo at the baseline";
      # A switch leaves a running one be (see burrow.switch's check).
      restartIfChanged = false;
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      path = [ pkgs.gh pkgs.git pkgs.gnutar pkgs.coreutils config.burrow.tools.proj ];
      serviceConfig = {
        Type = "oneshot";
        User = user;
        ExecStart = "${steward}/bin/autoland-steward${lib.optionalString cfg.dryRun " status"}";
        TimeoutStartSec = "20min";
        Nice = 10;
      };
    };
    # The reviews the passes queue, as soon as there are any, as the
    # reviewers' own user: no GitHub login, and the snapshots are all it
    # can read of the repos.
    systemd.services.autoland-review = lib.mkIf (!cfg.dryRun) {
      description = "autoland: the reviewers' reviews of the queued PR heads";
      # A switch leaves a running one be (see burrow.switch's check).
      restartIfChanged = false;
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      # What the agents read the snapshot with, and bubblewrap for codex's
      # own sandbox inside theirs.
      path = with pkgs; [ bash git bubblewrap coreutils findutils gnugrep gnused diffutils ripgrep ];
      environment.HOME = reviewerHome;
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
        ReadWritePaths = [ reviewerHome "${reviewDir}/queue" "${reviewDir}/done" ];
      } // lib.optionalAttrs rv.deepseek.enable {
        LoadCredential = "deepseek:${rv.deepseek.envFile}";
      };
    };
    systemd.paths.autoland-review = lib.mkIf (!cfg.dryRun) {
      wantedBy = [ "paths.target" ];
      pathConfig.DirectoryNotEmpty = "${reviewDir}/queue";
    };
    systemd.timers.autoland = {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnBootSec = "5min";
        OnUnitInactiveSec = cfg.every;
      };
    };

    # The steward's state, and the reviews: it queues them and takes the
    # snapshots; the reviewers' user reads those, answers in done/ and takes
    # what it answered off the queue. The clones the snapshots come from are
    # the steward's alone.
    systemd.tmpfiles.settings.burrow = let
      dir = mode: group: { d = { inherit mode user group; }; };
    in {
      "${stateDir}/autoland" = dir "0755" account.group;
      ${reviewDir} = dir "2750" reviewer;
      "${reviewDir}/queue" = dir "2770" reviewer;
      "${reviewDir}/done" = dir "2770" reviewer;
      "${reviewDir}/snap" = dir "2750" reviewer;
      "${reviewDir}/git" = dir "0700" account.group;
    };

    burrow.claude.machineMd = lib.mkOrder 1050 ''
      ## Pull requests

      Every repo of ${lib.concatStringsSep ", " cfg.owners} merges its PRs on its own, through the
      steward (`autoland`, every ${cfg.every}). Open PRs ready for review, never as drafts (a
      draft is left alone until it's ready), then leave them be:

      - ${reviewerNames} review every head; the steward posts their reviews under
        ${config.burrow.owner}'s login, each thread marked as its reviewer's. Fix or answer
        every finding (reply on the thread, push the fix) and resolve the thread; the
        reviewer that raised it looks again once they're all resolved.
      - Once they all find nothing on the exact head, every thread is resolved and CI is
        green, it posts `${cfg.context}` and merges it, `.github/` changes included; the
        branch goes too.
      - A settled PR that touches a one-way path (secrets, migrations, CODEOWNERS), or
        a repo with no CI, gets a Claude job that judges it: it lands what a revert or a
        follow-up can undo, and sends ${config.burrow.owner} only what can't be undone or is theirs to decide.
        The judge doesn't merge: the steward lands the PR on its verdict. Outside a
        steward job, a Claude session may merge such a PR itself (squash, delete the
        branch) once it has made that call.
      - ${config.burrow.owner}'s standing authority: merges, GitHub settings changes and cleanup
        are pre-approved. Take the judgment call first; only a genuine one-way door
        you can't decide waits for them, labelled `${cfg.labels.owner}` and listed in one issue
        (so are outside contributions and PRs past ${toString cfg.rounds} review rounds).
        `${cfg.labels.hold}` keeps the steward off a PR.
      - `autoland-steward status` shows where every open PR stands, changing nothing.
      - Every repo, new ones too, is kept at the baseline in burrow's
        docs/repo-baseline.md.
    '';
  };
}
