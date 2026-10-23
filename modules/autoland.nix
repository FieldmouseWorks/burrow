# autoland: every PR in every repo you own goes from draft to merged on its
# own, once a reviewer has nothing left to say (autoland/steward.py).
#
#   draft -> Codex reviews -> Claude fixes or answers every finding
#         -> Codex finds nothing on the exact head, CI green
#         -> `review-settled` status -> ready -> auto-merge -> branch deleted
#
# The owner only sees what needs them, in one issue: one-way doors, PRs past
# the round limit, outside contributions, anything stuck. It also keeps every
# repo's settings at the baseline (docs/repo-baseline.md), new repos included.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.autoland;
  inherit (config.burrow) user stateDir;
  account = config.users.users.${user};
  inherit (lib) mkOption types;

  settings = {
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
      actionBumps = true;
    };
    repos = cfg.repos;
    baselineTask = ''
      REPO is new, and doesn't meet the baseline every repo of ${config.burrow.owner}'s keeps yet. Read
      ${../docs/repo-baseline.md} and bring this repo up to its "Files" section: open one draft PR that adds
      what's missing (CI that builds and runs the tests, AGENTS.md with the shared "Pull requests" and
      "Code Review Rules" sections, CLAUDE.md, README, .gitignore, LICENSE if it's public) and changes
      nothing else. Don't ask for a review or merge it: the steward does that.
    '';
  };

  configFile = pkgs.writeText "autoland.json" (builtins.toJSON settings);
  stewardPy = pkgs.writers.writePython3Bin "autoland-steward"
    { flakeIgnore = [ "E501" "W503" ]; }
    (builtins.readFile ./autoland/steward.py);
  # The command, with this machine's config, so `autoland-steward status`
  # works from any shell, not only in the service.
  steward = pkgs.writeShellScriptBin "autoland-steward" ''
    export AUTOLAND_CONFIG=''${AUTOLAND_CONFIG:-${configFile}}
    exec ${stewardPy}/bin/autoland-steward "$@"
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
      default = "2min";
      description = "Time between passes (after one ends).";
    };

    rounds = mkOption {
      type = types.ints.positive;
      default = 5;
      description = "Codex rounds with findings before it stops and asks you.";
    };

    oneWay = mkOption {
      type = types.listOf types.str;
      default = [ "^\\.github/" "(^|/)secrets?/" "(^|/)\\.sops\\.yaml$" "\\.sops\\.(ya?ml|json|env)$" "(^|/)migrations?/" ];
      description = ''
        Path regexes that make a PR yours to merge: it's settled the same way,
        then waits for you. The default is the gate itself (workflows,
        CODEOWNERS), secrets and database migrations.
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
        existing repos included) that opens a draft PR bringing its files up to
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

    repos = mkOption {
      type = types.attrsOf types.attrs;
      default = { };
      example = lib.literalExpression ''
        {
          # Its update pipeline pushes to main: rules stop force pushes and deletion only.
          "sam/homelab".directPush = true;
          "sam/website" = { oneWay = [ "^deploy/" ]; rounds = 3; };
        }
      '';
      description = ''
        Per-repo overrides (OWNER/NAME): any of rounds, oneWay,
        handoffAfter, settleDelay, codexTimeout (minutes), directPush,
        ruleset (false: no baseline ruleset), requireCi (false: a PR with no
        checks at all counts as green, for a repo with nothing to test; by
        default a settled PR in a repo with no CI waits for you),
        actionBumps (false: a dependency bot's bump of the actions pinned in
        .github/workflows waits for you like any other .github/ change; by
        default it merges on green CI, as other dependency updates do).
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    burrow.proj.enable = true;
    burrow.notify.enable = true;

    environment.systemPackages = [ steward ];

    systemd.services.autoland = {
      description = "autoland: settle and merge every PR, keep every repo at the baseline";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      path = [ pkgs.gh pkgs.git pkgs.coreutils config.burrow.tools.proj ];
      serviceConfig = {
        Type = "oneshot";
        User = user;
        ExecStart = "${steward}/bin/autoland-steward${lib.optionalString cfg.dryRun " status"}";
        TimeoutStartSec = "20min";
        Nice = 10;
      };
    };
    systemd.timers.autoland = {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnBootSec = "5min";
        OnUnitInactiveSec = cfg.every;
      };
    };

    systemd.tmpfiles.settings.burrow."${stateDir}/autoland".d = {
      mode = "0755";
      inherit user;
      group = account.group;
    };

    burrow.claude.machineMd = lib.mkOrder 1050 ''
      ## Pull requests

      Every repo of ${lib.concatStringsSep ", " cfg.owners} merges its PRs on its own, through the
      steward (`autoland`, every ${cfg.every}). Open PRs as drafts, then leave them be:

      - The steward asks Codex for a review. Fix or answer every finding (reply on
        the thread, push the fix) and resolve the thread; it asks Codex again once
        they're all resolved.
      - Once Codex finds nothing on the head, every thread is resolved and CI is
        green, it posts `${cfg.context}`, marks the PR ready and merges it; the
        branch goes too. Don't merge, mark ready or post that status yourself.
      - What it won't merge (paths matching its one-way list, such as `.github/`,
        secrets and migrations; more than ${toString cfg.rounds} Codex rounds; outside
        contributions; repos with no CI yet) waits for ${config.burrow.owner}, listed in one issue. Label a PR
        `${cfg.labels.owner}` to send it there yourself, or `${cfg.labels.hold}` to keep the
        steward off it.
      - `autoland-steward status` shows where every open PR stands, changing nothing.
      - Every repo, new ones too, is kept at the baseline in burrow's
        docs/repo-baseline.md.
    '';
  };
}
