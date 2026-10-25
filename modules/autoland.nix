# autoland: every PR in every repo you own goes from ready to merged on its
# own, once a reviewer has nothing left to say (autoland/steward.py).
#
#   ready -> Codex (and DeepSeek, if it's on) reviews
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
    codexLogin = "chatgpt-codex-connector";
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
      codexTimeout = cfg.codexTimeoutMinutes;
      directPush = false;
      ruleset = true;
      requireCi = true;
      proofJobs = { };
      deepseek = true;
    };
    repos = cfg.repos;
    # Every repo's hook to burrow-events (repos.py), with what each of its
    # handlers takes.
    webhook = {
      inherit (config.burrow.events) url;
      events = config.burrow.events.hookEvents;
    };
    # For DeepSeek's reviews coming back (deepseek.py).
    kick = "${config.burrow.events.kick}/autoland";
    deepseek = {
      inherit (cfg.deepseek) enable baseUrl effort skipPaths maxDiffChars parallel;
      # Its thinking counts against this too, and at max effort a real PR
      # thought past 131072 in most calls (remi#39), so it's the API's
      # ceiling. Only what it uses is paid for.
      maxTokens = 384000;
      # Per read: the API keeps a long call alive with blank lines.
      timeout = 600;
      caFile = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
    };
    baselineTask = ''
      REPO is new, and doesn't meet the baseline every repo of ${config.burrow.owner}'s keeps yet. Read
      ${baselineDocs}/docs/repo-baseline.md and bring this repo up to its "Files" section: open one PR, ready for review (not a draft), that adds
      what's missing (CI that builds and runs the tests through the repo's flake, AGENTS.md with the house block and the shared "Pull requests" and
      "Code Review Rules" sections, CLAUDE.md, README, .gitignore, LICENSE if it's public) and changes
      nothing else. Don't ask for a review or merge it: the steward does that.
    '';
  };

  dsAlso = lib.optionalString cfg.deepseek.enable
    ", and posts DeepSeek's (under ${config.burrow.owner}'s login, marked as DeepSeek's)";

  configFile = pkgs.writeText "autoland.json" (builtins.toJSON settings);
  # steward.py and the modules it imports from beside it, linted as they're
  # built.
  stewardSrc = pkgs.runCommand "autoland-steward-src"
    { nativeBuildInputs = [ pkgs.python3Packages.flake8 ]; } ''
    mkdir $out
    cp ${./autoland/steward.py} $out/steward.py
    cp ${./autoland/common.py} $out/common.py
    cp ${./autoland/deepseek.py} $out/deepseek.py
    cp ${./autoland/repos.py} $out/repos.py
    cp ${./autoland/tasks.py} $out/tasks.py
    cp ${./autoland/touches.py} $out/touches.py
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
    { key = "burrow/events"; imports = [ (import ./events.nix inputs) ]; }
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

    rounds = mkOption {
      type = types.ints.positive;
      default = 5;
      description = "Review rounds with findings (Codex's or DeepSeek's, whichever has had more) before it stops and asks you.";
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
      default = 10;
      description = ''
        How long open findings or red CI sit with nobody pushing or replying
        before a Claude job on this machine takes them. Usually the session
        that opened the PR fixes them first.
      '';
    };

    settleDelayMinutes = mkOption {
      type = types.ints.positive;
      default = 1;
      description = "How long a new head sits still before Codex is asked, so a burst of pushes gets one review.";
    };

    codexTimeoutMinutes = mkOption {
      type = types.ints.positive;
      default = 45;
      description = "How long a Codex request may go unanswered before it's asked again (twice unanswered: you hear about it).";
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

    deepseek = {
      enable = lib.mkEnableOption ''
        DeepSeek as a second reviewer, next to Codex. The steward sends each
        PR head's diff, with the Code Review Rules of the nearest AGENTS.md,
        to DeepSeek's API and posts its findings as one review (under your
        login, each thread marked as DeepSeek's), and a PR settles only once
        Codex and DeepSeek both find nothing on its head. Its rounds count
        towards `rounds` like Codex's. The diffs of every repo it covers go
        to DeepSeek, private ones included; `skipPaths` are left out
      '';

      envFile = mkOption {
        type = types.str;
        example = "/run/secrets/deepseek-review";
        description = ''
          A root-only file of KEY=value lines: DEEPSEEK_API_KEY, DEEPSEEK_MODEL
          (the API's model name: which model does what is the machine's
          business, so it lives here and not in a tracked file) and optionally
          DEEPSEEK_EFFORT, which overrides `effort`. Only the service reads it
          (systemd's LoadCredential).
        '';
      };

      baseUrl = mkOption {
        type = types.str;
        default = "https://api.deepseek.com";
        description = "The API's base URL (OpenAI-style chat completions).";
      };

      effort = mkOption {
        type = types.enum [ "low" "high" "max" ];
        default = "max";
        description = "How hard it thinks (`reasoning_effort`).";
      };

      skipPaths = mkOption {
        type = types.listOf types.str;
        default = [ "(^|/)secrets?/" "(^|/)\\.sops\\.yaml$" "\\.sops\\.(ya?ml|json|env)$" "(^|/)\\.env(\\..*)?$" "\\.(pem|key|p12|pfx)$" ];
        description = "Path regexes whose contents are never sent (it's told they changed, not how).";
      };

      maxDiffChars = mkOption {
        type = types.ints.positive;
        default = 800000;
        description = "A bigger diff isn't sent: the PR waits for you instead.";
      };

      parallel = mkOption {
        type = types.ints.positive;
        default = 4;
        description = ''
          At most this many DeepSeek reviews at once. A careful one can take
          DeepSeek half an hour, so they run in their own service
          (autoland-deepseek), apart from the steward's passes.
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
        handoffAfter, settleDelay, codexTimeout (minutes), directPush,
        ruleset (false: no baseline ruleset), requireCi (false: a PR with no
        checks at all counts as green, for a repo with nothing to test; by
        default a Claude job judges a settled PR in a repo with no CI,
        building and testing it itself), proofJobs (checks that run on a
        machine rather than in CI, `{ "<check>" = "<how to run it and record
        its verdict>"; }`: once a settled PR waits on nothing else, one
        Claude job a head runs them on that commit and records each verdict,
        a failure as a failure), keepBranches
        (more branch globs the cleanup never touches in this repo, on top of
        the machine's, for branches a repo keeps as evidence), deepseek
        (false: Codex alone reviews this repo, and none of it goes to
        DeepSeek).
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    burrow.proj.enable = true;
    burrow.notify.enable = true;

    environment.systemPackages = [ steward ];

    # Every PR's pass comes from what happens to it (burrow.events): a push, a
    # review, a comment or CI finishing on it starts its repo's pass
    # (autoland@OWNER-REPO), and so do a Claude job's turn ending and DeepSeek's
    # review coming back. A pass that waits on time (a new head's settle
    # delay, a reviewer's timeout) books its repo's next one in wake.json.
    # The full pass (settings, new repos, branches) is burrow-events'
    # catch-up, when it starts and hourly.
    burrow.events.handlers.autoland = {
      # The PR itself (opened, ready, labels, retargeted), pushes, reviews and
      # their threads, comments (Codex's summary), CI finishing, and commit
      # statuses (a proof's verdict).
      events = [ "push" "pull_request" "pull_request_review" "pull_request_review_comment"
        "pull_request_review_thread" "issue_comment" "check_suite" "status" ];
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
      path = [ pkgs.gh pkgs.git pkgs.coreutils config.burrow.tools.proj ];
      serviceConfig = {
        Type = "oneshot";
        User = user;
        ExecStart = "${steward}/bin/autoland-steward"
          + (if cfg.dryRun then " status" else lib.optionalString (name == "autoland@") " kick %I");
        TimeoutStartSec = "20min";
        Nice = 10;
        # For the repos' hooks (repos.py).
        LoadCredential = "webhook:${config.burrow.events.secretFile}";
      };
    }) // {
      # The DeepSeek reviews the passes queue, as soon as there are any.
      autoland-deepseek = lib.mkIf (cfg.deepseek.enable && !cfg.dryRun) {
        description = "autoland: DeepSeek's reviews of the queued PR heads";
        # A switch leaves a running one be (see burrow.switch's check).
        restartIfChanged = false;
        after = [ "network-online.target" ];
        wants = [ "network-online.target" ];
        path = [ pkgs.gh pkgs.coreutils ];
        serviceConfig = {
          Type = "oneshot";
          User = user;
          ExecStart = "${steward}/bin/autoland-steward deepseek";
          # However long DeepSeek thinks: each read has its own timeout, and the
          # API keeps a long call alive.
          TimeoutStartSec = "infinity";
          Nice = 10;
          LoadCredential = "deepseek:${cfg.deepseek.envFile}";
        };
      };
    };
    systemd.paths.autoland-deepseek = lib.mkIf (cfg.deepseek.enable && !cfg.dryRun) {
      wantedBy = [ "paths.target" ];
      pathConfig.DirectoryNotEmpty = "${stateDir}/autoland/ds-queue";
    };

    systemd.tmpfiles.settings.burrow = lib.genAttrs
      [ "${stateDir}/autoland" "${stateDir}/autoland/ds-queue" "${stateDir}/autoland/ds-done" ]
      (_: {
        d = {
          mode = "0755";
          inherit user;
          group = account.group;
        };
      });

    burrow.claude.machineMd = lib.mkOrder 1050 ''
      ## Pull requests

      Every repo of ${lib.concatStringsSep ", " cfg.owners} merges its PRs on its own, through the
      steward (`autoland`, on every change to a PR). Open PRs ready for review, never as drafts (a
      draft is left alone until it's ready), then leave them be:

      - The steward asks Codex for a review${dsAlso}. Fix or answer every finding (reply on
        the thread, push the fix) and resolve the thread; the reviewer that raised it
        looks again once they're all resolved.
      - Once ${if cfg.deepseek.enable then "Codex and DeepSeek both find" else "Codex finds"} nothing on the exact head, every thread is resolved and CI is
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
