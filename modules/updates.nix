# Staying current, top to bottom, without anyone asking.
#
#   - HOST-update: on each push to the flake's main (a merged PR) or to an
#     input it watches, and hourly for the ones that send no event (nixpkgs,
#     Claude Code), bump the flake's inputs and Claude Code, build, VM-test,
#     switch with the dead man's switch, health-check, confirm, push to main.
#     Breakage reverts itself and goes to a Claude job.
#   - Renovate: dependency PRs for every repo you own, merged on green CI;
#     renovate-sweep gives the stuck ones to a Claude job. A repo's push to
#     its default branch, or its Renovate PRs moving or finishing CI, start
#     both for that repo (burrow.events); all of them go hourly, for the
#     releases upstream that send no event.
#   - advisory-watch: new CVEs in what the box and its projects run, hourly,
#     each one handed to a Claude job the same hour.
#   - firmware-check: firmware on offer goes to the owner (flashing is theirs).
#   - rustup: the user's Rust toolchains, daily.
#
# Kernels get their own trial boot: burrow.switch.trialBoot.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.updates;
  inherit (config.burrow) prefix user stateDir tools;
  inherit (config.burrow.github) machineApp machineGh;
  account = config.users.users.${user};
  system = pkgs.stdenv.hostPlatform.system;
  update = "${prefix}-update";
  health = "${prefix}-health";
  project = baseNameOf config.burrow.flake;
  # The origin's parts, as git reads a URL: scheme://[user@]host[:port]/path,
  # or scp-style [user@]host:path. Its host is compared as DNS compares one
  # (any case, a trailing dot or not) and its port only when it isn't the
  # scheme's default. OWNER/NAME, when the flake lives on GitHub over HTTPS or
  # SSH (either form), with or without .git and a trailing slash. An origin
  # whose host is github.com, or anything under it, and that isn't read here
  # is refused (the assertion below): a repo's train is never off only because
  # its URL was spelled another way.
  origin =
    let
      o = cfg.origin;
      url = builtins.match "([A-Za-z][A-Za-z0-9+.-]*)://([^/]*)(/(.*))?" o;
      scp = builtins.match "([^/:]*):(.*)" o;
      parts =
        if url != null then { scheme = lib.toLower (lib.elemAt url 0); authority = lib.elemAt url 1; path = toString (lib.elemAt url 3); }
        else if scp != null then { scheme = "scp"; authority = lib.elemAt scp 0; path = lib.elemAt scp 1; }
        else null;
      a = builtins.match "(([^@]*)@)?([^:]*)(:([0-9]*))?" parts.authority;
      host = lib.removeSuffix "." (lib.toLower (lib.elemAt a 2));
      user = lib.elemAt a 1;
      port = lib.elemAt a 4;
      default = { https = "443"; ssh = "22"; }.${parts.scheme} or null;
      name = builtins.match "([^/]+/[^/]+)" (lib.removeSuffix ".git" (lib.removeSuffix "/" parts.path));
      read = a != null && host == "github.com" && name != null
        && (port == null || port == default)
        && (if parts.scheme == "https" then user == null else builtins.elem parts.scheme [ "ssh" "scp" ] && user == "git");
    in {
      onGithub = parts != null && a != null && (host == "github.com" || lib.hasSuffix ".github.com" host);
      repo = if parts != null && read then lib.elemAt name 0 else null;
    };
  githubRepo = origin.repo;
  # Its PRs land through its merge train (burrow.autoland's `train`): then
  # nothing here pushes to its main, and what would goes up as a PR. GitHub
  # reads names whatever their case, so autoland's repos are matched that way.
  autoland = lib.attrByPath [ "burrow" "autoland" ] { } config;
  autolandRepo = lib.findFirst (n: githubRepo != null && lib.toLower n == lib.toLower githubRepo) null
    (lib.attrNames (autoland.repos or { }));
  train = cfg.enable && autolandRepo != null && (autoland.enable or false)
    && (autoland.repos.${autolandRepo}.train or false);
  # Every repo whose PRs land through its train: Renovate merges none there.
  trainRepos = lib.optionals (autoland.enable or false)
    (lib.attrNames (lib.filterAttrs (_: r: r.train or false) (autoland.repos or { })));

  # The longest a Renovate run goes on (seconds): renovate-sweep waits that
  # long after taking a refresh request back before a job may push.
  renovateRunFor = 2 * 3600;

  vars = {
    UPDATE = update;
    HEALTH = health;
    HOST = config.networking.hostName;
    PROJECT = project;
    DIR = stateDir;
    WORK = "${config.burrow.scratchDir}/${update}";
    SRC = config.burrow.projectsDir;
    FLAKE = config.burrow.flake;
    ORIGIN = cfg.origin;
    TRAIN = if train then "1" else "";
    # Where HOST-update's own PR comes from, in a train repo (updates/propose.sh).
    REPO = if githubRepo == null then "" else githubRepo;
    BRANCH = update;
    CONFIG = config.burrow.switch.configName;
    SYSTEM = system;
    MANIFEST = if cfg.claudeManifest == null then "" else cfg.claudeManifest;
    CHECKS = lib.concatStringsSep " " cfg.checks;
    # burrow.cpu's slots, which the VM tests wait for where the machine has them.
    HEAVY = if config.burrow.cpu.enable then "${tools.heavy}/bin/heavy" else "";
    WITHIN = cfg.confirmWithin;
    CHANNEL = "nixos-${config.system.nixos.release}";
    OWNER = config.burrow.owner;
    BURROW_USER = user;
    BURROW_GROUP = account.group;
    HOME_DIR = account.home;
    USER_UID = toString account.uid;
    SWITCH = "${prefix}-switch";
    SWITCH_STATE = "/run/${prefix}-switch";
    CONFIRM = "${prefix}-confirm";
    BOOT = if config.burrow.switch.trialBoot.enable then "${prefix}-boot" else "";
    NOTIFY = "${prefix}-notify";
    REVIEW_HINTS = cfg.firmware.reviewHints;
    # The autoland steward's commit status, which renovate-sweep doesn't count as CI.
    SETTLED = config.burrow.autoland.context or "review-settled";
    # In a train repo, the steward's App as GraphQL names its bot, and the
    # train's status, which counts as CI only from that App (updates/propose.sh).
    STEWARD = if train then autoland.app.slug else "";
    TRAIN_CONTEXT = if train then config.burrow.ledger.trainContext else "";
    # The repos the steward covers, as it last found them; none without it.
    COVERED = if config.burrow.autoland.enable or false then "${stateDir}/autoland/repos.json" else "";
    SEAL = "${majorSeal}/bin/major-seal";
    # When a lock file refresh lock-age refused for age only can go again.
    LOCK_WAIT = "${lockWait}/bin/lock-wait";
    RENOVATE_RUN = toString renovateRunFor;
    # The App the jobs these start act as (proj run --app), and its bot as
    # GraphQL names it: none without burrow.github.machinery.
    APP = if machineApp == null then "" else machineApp.name;
    BOT = if machineApp == null then "" else machineApp.slug;
    APP_ENV = if machineApp == null then "" else machineApp.envFile;
    # Renovate's (updates/renovate.sh).
    APP_OWNERS = lib.concatStringsSep " " cfg.renovate.owners;
    COVERS = coversTool;
    RUN_LOCK = "${cfg.renovate.dir}/run.lock";
    RUNS = "${cfg.renovate.dir}/runs";
  };

  # Each script gets the names it uses (shellcheck rejects the rest), set above
  # its own text.
  uses = text: n: lib.length (builtins.split "\\$\\{?${n}([^A-Za-z0-9_]|$)" text) > 1;
  script = name: files: runtimeInputs:
    let text = lib.concatMapStrings builtins.readFile (lib.toList files);
    in pkgs.writeShellApplication {
      inherit name runtimeInputs;
      text = lib.concatMapStrings (n: "${n}=${lib.escapeShellArg vars.${n}}\n")
        (lib.filter (uses text) (lib.attrNames vars)) + text;
    };

  calls = [ tools.proj tools.notify ];
  healthTool = script health ./updates/health.sh (with pkgs; [ coreutils gnugrep gnused gawk systemd ]);
  # Its own functions first (they run only once update.sh has defined what they call).
  updateTool = script update [ ./updates/dispatch.sh ./updates/running.sh ./updates/propose.sh ./updates/push.sh ./updates/check.sh ./updates/update.sh ] (
    (with pkgs; [ coreutils jq curl util-linux systemd gnugrep gnused gawk vulnix config.nix.package ])
    ++ [ config.programs.git.package healthTool tools.switch tools.confirm machineGh ] ++ calls ++ lib.optional (tools ? boot) tools.boot
  );
  advisoryWatch = script "advisory-watch" ./updates/advisory-watch.sh (
    (with pkgs; [ coreutils curl jq gnugrep gnused gawk util-linux vulnix osv-scanner config.nix.package ]) ++ calls
  );
  firmwareCheck = script "firmware-check" ./updates/firmware-check.sh (
    (with pkgs; [ coreutils jq gnugrep gnused fwupd dmidecode nvme-cli util-linux ]) ++ calls
  );
  # What seals renovate-sweep's verdicts on majors (updates/major-seal.py),
  # with the seal the autoland steward checks them with beside it, linted as
  # it's built.
  majorSealSrc = pkgs.runCommand "major-seal-src" { nativeBuildInputs = [ pkgs.python3Packages.flake8 ]; } ''
    mkdir $out
    cp ${./updates/major-seal.py} $out/major-seal.py
    cp ${./autoland/seal.py} $out/seal.py
    flake8 --ignore E501,W503 $out
  '';
  # What holds a Cargo or npm lock file refresh to the release age
  # (updates/lock-age.py), linted as it's built.
  lockAgeSrc = pkgs.runCommand "lock-age-src" { nativeBuildInputs = [ pkgs.python3Packages.flake8 ]; } ''
    mkdir $out
    cp ${./updates/lock-age.py} $out/lock-age.py
    flake8 --ignore E501,W503 $out
  '';
  lockAgePython = pkgs.python3.withPackages (p: [ p.json5 ]);
  # Every release age a Renovate config sets, its presets resolved by
  # Renovate's own code (updates/release-ages.mjs).
  releaseAges = pkgs.writeShellScriptBin "release-ages" ''
    exec ${lib.getExe pkgs.nodejs_24} ${./updates/release-ages.mjs} ${pkgs.renovate}
  '';
  # lock-age holds Cargo and npm refreshes to the longest age the machine's
  # policy and the repo's own config set, presets and all. It takes the
  # policy from the store, so the command Renovate runs has only the branch
  # the refresh is for, which Renovate fills in: a ref name has no spaces,
  # and one with anything but these characters isn't allowed to run.
  lockAge = pkgs.writeShellScriptBin "lock-age" ''
    exec ${lockAgePython}/bin/python3 -I ${lockAgeSrc}/lock-age.py \
      ${pkgs.writeText "lock-age-policy.json" (builtins.toJSON renovatePolicy)} ${releaseAges}/bin/release-ages "$@"
  '';
  lockAgeCommand = "${lockAge}/bin/lock-age {{{baseBranch}}}";
  lockAgeAllowed = "^${lib.escapeRegex "${lockAge}/bin/lock-age"} [A-Za-z0-9._/+-]+$";
  # Cargo's and npm's lock file refreshes resolve to registry releases, with
  # no cutoff Renovate keeps (cargo has none; npm's --before is dropped
  # whenever it fails to resolve). lock-age holds them to the age itself:
  # each release a refresh brings in is taken back to one old enough, or the
  # refresh fails as an artifact error and doesn't merge (burrow#82). So
  # the stability check needn't wait for a timestamp they don't have. Last,
  # so no rule in settings undoes it.
  lockAgeRule = {
    matchUpdateTypes = [ "lockFileMaintenance" ];
    matchManagers = [ "cargo" "npm" ];
    minimumReleaseAgeBehaviour = "timestamp-optional";
    postUpgradeTasks = {
      commands = [ lockAgeCommand ];
      fileFilters = [ "**/Cargo.lock" "**/package-lock.json" ];
      executionMode = "branch";
    };
  };
  # What reads lock-age's refusal for age in Renovate's comment
  # (autoland/lock_wait.py, the steward's too), linted as it's built.
  lockWaitSrc = pkgs.runCommand "lock-wait-src" { nativeBuildInputs = [ pkgs.python3Packages.flake8 ]; } ''
    mkdir $out
    cp ${./autoland/lock_wait.py} $out/lock_wait.py
    flake8 --ignore E501,W503 $out
  '';
  lockWait = pkgs.writeShellScriptBin "lock-wait" ''
    exec ${pkgs.python3}/bin/python3 -I ${lockWaitSrc}/lock_wait.py "$@"
  '';
  majorSeal = pkgs.writeShellScriptBin "major-seal" ''
    exec ${pkgs.python3}/bin/python3 -E -s ${majorSealSrc}/major-seal.py ${stateDir}/renovate-sweep "$@"
  '';
  renovateRun = script "renovate-run" ./updates/renovate.sh (
    [ pkgs.renovate machineGh pkgs.util-linux pkgs.coreutils ] ++ lib.optional (machineApp != null) tools.gh-token
  );
  renovateSweep = script "renovate-sweep" ./updates/renovate-sweep.sh (
    (with pkgs; [ coreutils findutils jq gnused ]) ++ [ config.programs.git.package machineGh ] ++ calls
  );

  # The ZFS pools the machine needs imported, as NixOS's zfs module decides
  # it: the ones its file systems mount from (a pool only noauto mounts use
  # can be away), and the ones it imports besides, by name or GUID. ZFS support
  # alone, with none, has none to check.
  zfsPools = lib.unique (map (fs: lib.head (lib.splitString "/" fs.device))
    (lib.filter (fs: fs.fsType == "zfs" && !lib.elem "noauto" fs.options) config.system.build.fileSystems)
    ++ config.boot.zfs.extraPools);

  # What the health checks may call, beyond the system's own PATH.
  checkPath = lib.makeBinPath ((with pkgs; [ coreutils systemd curl jq getent util-linux gnugrep ])
    ++ lib.optional config.services.tailscale.enable config.services.tailscale.package
    ++ lib.optional (zfsPools != [ ]) config.boot.zfs.package);

  # Renovate's settings for every repo (a repo's own renovate.json can add to
  # them). Checked by renovate-config-validator at build time.
  rcfg = cfg.renovate;
  # Renovate takes a branch with a commit by anyone but its gitAuthor as edited
  # by hand, and leaves it alone. With the user's own email, the fixes Claude
  # pushes to its branches (as the user) look like Renovate's, and its next
  # rebase drops them.
  gitConfigs = let c = config.programs.git.config; in if builtins.isList c then c else [ c ];
  userEmails = lib.concatMap (c: lib.optional (c ? user && c.user ? email) c.user.email) gitConfigs;
  renovateEmail = let m = if rcfg.gitAuthor == null then null else builtins.match ".*<([^>]*)>.*" rcfg.gitAuthor;
    in if m == null then null else lib.head m;
  # Renovate's policy, settings included, before lock-age goes in
  # (renovateSettings).
  renovatePolicy = lib.recursiveUpdate ({
    platform = "github";
    autodiscover = true;
    autodiscoverFilter = rcfg.autodiscoverFilter;
    onboarding = false;
    requireConfig = "optional";
    # Each repo's issue listing what Renovate hasn't opened a PR for, and
    # why: the autoland steward reads it for what's stuck there (its
    # dashboard option).
    dependencyDashboard = true;
    binarySource = "global";
    baseDir = "${rcfg.dir}/work";
    cacheDir = "${rcfg.dir}/cache";
    repositoryCache = "enabled";
    extends = [ "config:recommended" "helpers:pinGitHubActionDigests" ];
    prHourlyLimit = 0;

    # Commit subjects and PR titles in the house style, `area: plain
    # sentence` (house/rules.md): "deps: update foo to v1.5". The PR title is
    # the subject the steward squashes onto main, and house-check fails a PR
    # whose title isn't. Left to itself Renovate guesses from each repo's
    # history, and config:recommended's package rules type it fix or chore,
    # so a package rule below has the final word on the type.
    semanticCommits = "enabled";
    semanticCommitType = "deps";
    semanticCommitScope = null;
    prConcurrentLimit = 20;

    # Three days before a release counts: hijacked npm and crates releases are
    # usually caught and pulled within that. Security fixes don't wait.
    minimumReleaseAge = "3 days";
    internalChecksFilter = "strict";
    osvVulnerabilityAlerts = true;
    vulnerabilityAlerts = {
      enabled = true;
      labels = [ "security" ];
      minimumReleaseAge = null;
      automerge = true;
      schedule = [ "at any time" ];
    };

    # Transitive fixes arrive through a fresh lockfile, daily (but see the
    # lock file rules below).
    lockFileMaintenance = {
      enabled = true;
      schedule = [ "before 5am" ];
      automerge = true;
    };

    # Merged by Renovate itself once every check is green, so it doesn't
    # depend on each repo's auto-merge setting.
    automergeType = "pr";
    platformAutomerge = false;

    # Rebase only on a conflict. By default Renovate rebases every automerge
    # PR whenever its base moves, and each rebase restarts CI. On a repo with
    # one slow gate slot (LoK-web) every merge to main sent all open PRs,
    # security fixes included, back to the end of the queue, so none finished.
    # Main's own CI still runs after each merge. A repo whose branch rules
    # require PRs to be up to date would leave them green but unmerged, and
    # renovate-sweep reports security PRs stuck that way.
    rebaseWhen = "conflicted";

    # The rules below take a lock file refresh without a release timestamp on
    # purpose, and each time Renovate warns that it did. A warning lands on the
    # dashboard's Repository Problems, where nothing can clear it and the
    # steward takes it for something stuck. Only the warning for an upgrade
    # that went ahead: any other still counts.
    logLevelRemap = [
      {
        matchMessage = "/^Some upgrade\\(s\\) did not have a releaseTimestamp, but as we're running with minimumReleaseAgeBehaviour=timestamp-optional, proceeding\\./";
        newLogLevel = "debug";
      }
    ];

    nix.enabled = true;

    packageRules = [
      {
        # pinDigest is a commit pin put on what had none (an action's `@v4`,
        # a workflow's `@main`): its PR is green and unmerged for good
        # otherwise, since the steward leaves Renovate's PRs to Renovate.
        matchUpdateTypes = [ "minor" "patch" "pin" "pinDigest" "digest" "lockFileMaintenance" ];
        automerge = true;
      }
      {
        # The owner merges these; renovate-sweep has a Claude make them green first.
        matchUpdateTypes = [ "major" ];
        automerge = false;
        labels = [ "major" ];
      }
      # One PR per ecosystem per repo for everything that isn't major.
      { matchCategories = [ "rust" ]; matchUpdateTypes = [ "minor" "patch" ]; groupName = "Rust crates"; }
      { matchCategories = [ "js" ]; matchUpdateTypes = [ "minor" "patch" ]; groupName = "npm packages"; }
      { matchCategories = [ "python" ]; matchUpdateTypes = [ "minor" "patch" ]; groupName = "Python packages"; }
      { matchCategories = [ "ci" ]; matchUpdateTypes = [ "minor" "patch" "digest" "pin" ]; groupName = "CI actions"; }
      { matchCategories = [ "nix" ]; groupName = "Nix inputs"; }
      {
        # One lock file refresh per manager, so one that can't merge (below)
        # holds up no other.
        matchUpdateTypes = [ "lockFileMaintenance" ];
        additionalBranchPrefix = "{{manager}}-";
        commitMessageExtra = "({{manager}})";
      }
      {
        # A lock file refresh has no release timestamp, and Renovate (42 on)
        # holds an update without one at a pending stability check for
        # good. Nix's is taken as stable without one: its inputs are
        # commits, not registry releases, and HOST-update already deploys
        # them the hour they land.
        matchUpdateTypes = [ "lockFileMaintenance" ];
        matchManagers = [ "nix" ];
        minimumReleaseAgeBehaviour = "timestamp-optional";
      }
      {
        # config:recommended's own rules type a dependency fix and the rest
        # chore; in the house style every update is "deps:".
        matchPackageNames = [ "*" ];
        semanticCommitType = "deps";
      }
    ] ++ lib.optional (rcfg.trustedOwners != [ ]) {
      # A dependency on a repo of trustedOwners' (a pinned workflow, an
      # action, a flake input) waits for nothing: the soak is against someone
      # else's hijacked release, and on a repo that's still moving it never
      # ends. Each new commit restarts the three days, so a pin on burrow's
      # main sat in "pending status checks" while burrow merged daily. Only
      # where the source URL is the repo the datasource fetches from (GitHub's
      # own, git's): a registry's package (a Docker image, an npm package)
      # claims whatever source URL it likes, and an OWNER/NAME on another host
      # is someone else's.
      matchDatasources = [ "github-tags" "github-digest" "github-releases" "git-refs" "git-tags" ];
      matchSourceUrls = map (o: "https://github.com/${o}/*") rcfg.trustedOwners;
      minimumReleaseAge = null;
    } ++ lib.optional (githubRepo != null) {
      # The machine's own flake.lock is HOST-update's: it deploys what it bumps.
      matchRepositories = [ githubRepo ];
      matchManagers = [ "nix" ];
      enabled = false;
    };

    # rust-toolchain.toml pins follow Rust's releases too.
    customManagers = [
      {
        customType = "regex";
        managerFilePatterns = [ "/(^|/)rust-toolchain\\.toml$/" ];
        matchStrings = [ "channel\\s*=\\s*\"(?<currentValue>\\d+\\.\\d+(\\.\\d+)?)\"" ];
        depNameTemplate = "rust";
        packageNameTemplate = "rust-lang/rust";
        datasourceTemplate = "github-releases";
      }
    ];
  } // lib.optionalAttrs (rcfg.timezone != null) { inherit (rcfg) timezone; }
    // lib.optionalAttrs (rcfg.gitAuthor != null) { inherit (rcfg) gitAuthor; })
    rcfg.settings;
  renovateSettings = renovatePolicy // {
    # The one command a repo's refresh may run after it (lock-age).
    allowedCommands = (renovatePolicy.allowedCommands or [ ]) ++ [ lockAgeAllowed ];
    packageRules = (renovatePolicy.packageRules or [ ]) ++ [ lockAgeRule ] ++ lib.optional (trainRepos != [ ]) {
      # The steward merges these, through the train onto the default branch,
      # and the rules there refuse anyone else's merge: Renovate only opens them. Last, so it has the
      # final word over every rule above. A security fix keeps its own
      # automerge (Renovate applies vulnerabilityAlerts after every package
      # rule), and the same rules refuse it.
      matchRepositories = trainRepos;
      automerge = false;
    };
  };

  # Whether Renovate's autodiscoverFilter takes a repo, by Renovate's own code
  # (updates/renovate-covers.mjs), on the node it runs on.
  coversTool = pkgs.writeShellScript "renovate-covers" ''
    exec ${lib.escapeShellArgs [
      (lib.getExe pkgs.nodejs_24) "${./updates/renovate-covers.mjs}" "${pkgs.renovate}" (builtins.toJSON renovateSettings.autodiscoverFilter)
    ]} "$@"
  '';

  renovateConfig = pkgs.runCommand "renovate-config.json"
    {
      nativeBuildInputs = [ pkgs.jq pkgs.renovate ];
      value = builtins.toJSON renovateSettings;
      passAsFile = [ "value" ];
      preferLocalBuild = true;
    }
    ''
      jq . "$valuePath" > $out
      renovate-config-validator $out
    '';

  online = {
    after = [ "network-online.target" ];
    wants = [ "network-online.target" ];
  };
in
{
  imports = [
    (import ./common.nix inputs)
    { key = "burrow/cpu"; imports = [ (import ./cpu.nix inputs) ]; }
    { key = "burrow/notify"; imports = [ (import ./notify.nix inputs) ]; }
    { key = "burrow/switch"; imports = [ (import ./switch.nix inputs) ]; }
    { key = "burrow/proj"; imports = [ (import ./proj.nix inputs) ]; }
    { key = "burrow/events"; imports = [ (import ./events.nix inputs) ]; }
    { key = "burrow/github"; imports = [ (import ./github.nix inputs) ]; }
  ];

  options.burrow.updates = {
    enable = lib.mkEnableOption "HOST-update and the rest of the update pipeline";

    repo = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      readOnly = true;
      default = githubRepo;
      defaultText = lib.literalMD "OWNER/NAME of `origin`, when it's on GitHub";
      description = "The flake's repo on GitHub (OWNER/NAME); null when `origin` isn't there.";
    };

    origin = lib.mkOption {
      type = lib.types.str;
      example = "https://github.com/sam/homelab";
      description = ''
        Where the machine's flake lives (git URL; on GitHub, any of the forms
        GitHub gives: HTTPS, `git@github.com:` or `ssh://`). HOST-update
        deploys its main branch and pushes the bumps it tested back there, with
        the user's git login: as a PR from its own branch, HOST-update, when
        the repo's PRs land through its merge train (`throughTrain`).
      '';
    };

    throughTrain = lib.mkOption {
      type = lib.types.bool;
      readOnly = true;
      default = train;
      defaultText = lib.literalMD "whether `origin`'s repo has `train` on in burrow.autoland.repos";
      description = ''
        Whether the flake's main moves only through its merge train, so
        nothing on the machine pushes to it: what would goes up as a PR.
      '';
    };

    checks = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "rehearsal" ];
      description = ''
        The flake's checks (VM tests) an update must pass before it goes live.
        A test that fails and then passes on the same tree counts as flaky: the
        update goes ahead and a Claude job fixes the test. One that fails on
        main as well holds updates back unless they fix a CVSS 7+ vulnerability
        in what runs now; those go live under the switch and health check.
      '';
    };

    confirmWithin = lib.mkOption {
      type = lib.types.str;
      default = "15min";
      description = "The dead man's switch window for an update (it health-checks for up to 3 minutes in it).";
    };

    watch = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = lib.optional (githubRepo != null) githubRepo;
      defaultText = lib.literalExpression "[ OWNER/NAME of origin, when it's on GitHub ]";
      example = [ "sam/homelab" "sam/burrow" ];
      description = ''
        GitHub repos whose pushes to their default branch start HOST-update
        (burrow.events, which needs their webhooks, as autoland keeps them):
        the flake's own, so a merged PR goes live at once, and the inputs
        that live on GitHub. Everything else (nixpkgs, Claude Code) it looks
        for hourly, in burrow-events' catch-up.
      '';
    };

    claudeManifest = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "claude-code.json";
      description = ''
        burrow.claude.manifest's path inside the flake's checkout. HOST-update
        bumps it to Claude Code's newest release along with the flake inputs.
        null: Claude Code comes with nixpkgs-unstable.
      '';
    };

    health = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      default = { };
      example = { website = "curl -fsS -o /dev/null https://example.com/"; };
      description = ''
        Shell commands that exit 0 while the machine is healthy, by name.
        HOST-update compares them before and after every switch: one that passed
        before and fails after reverts the switch, and burrow.heartbeat tells
        the owner about one that keeps failing. burrow adds its own (sshd, the
        nix daemon, the operator, burrow-events, the ZFS pools, DNS, GitHub,
        Claude Code...).
      '';
    };

    advisories.enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "advisory-watch: new CVEs (CISA KEV, vulnix, osv-scanner) go to Claude jobs, hourly.";
    };

    firmware = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "firmware-check: what fwupd offers goes to the owner daily, plus a monthly Claude review.";
      };
      reviewHints = lib.mkOption {
        type = lib.types.str;
        default = "the board vendor's support page, the NVMe vendor's, Intel's or AMD's security advisories for the ME or PSP and microcode";
        example = "Gigabyte's page for the board, Samsung's for the 990 PRO, AMD's advisories for the PSP and microcode";
        description = "Where the monthly review should look for firmware LVFS doesn't carry.";
      };
    };

    rustup.enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "`rustup update` daily for the user, when they have toolchains in ~/.rustup.";
    };

    renovate = {
      enable = lib.mkEnableOption ''
        a self-hosted Renovate for every repo it can see: as the machine's own
        App (burrow.github.machinery), else as the user, with gh's login'';
      autodiscoverFilter = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        example = [ "sam/*" ];
        description = "Which repos Renovate covers (new ones included).";
      };
      owners = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = lib.optionals (config.burrow.autoland.enable or false) config.burrow.autoland.owners;
        defaultText = lib.literalExpression "burrow.autoland.owners, when autoland is on";
        example = [ "sam" "sams-org" ];
        description = ''
          As the machine's own App: the GitHub users and orgs its installations
          are on, whose repos a full pass looks through (autodiscoverFilter
          still says which it covers). An App's token is one installation's,
          so each repo's run gets its owner's.
        '';
      };
      trustedOwners = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = lib.optionals (config.burrow.autoland.enable or false) config.burrow.autoland.owners;
        defaultText = lib.literalExpression "burrow.autoland.owners, when autoland is on";
        example = [ "sam" "sams-org" ];
        description = ''
          GitHub users and orgs whose repos are yours: an update to one of
          their repos on github.com (a pinned action or workflow, a git
          dependency) goes ahead without the three days' soak other releases
          get.
        '';
      };
      gitAuthor = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "renovate <renovate@example.com>";
        description = ''
          Who Renovate's commits are by. Not the user's own email: Renovate
          keeps a branch that has a commit by someone other than this, and
          rebases over everything else, so fixes pushed as the user would be
          lost.
        '';
      };
      timezone = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = config.time.timeZone;
        defaultText = lib.literalExpression "config.time.timeZone";
        description = "For Renovate's schedules.";
      };
      dir = lib.mkOption {
        type = lib.types.str;
        default = "/var/cache/renovate";
        description = "Renovate's clones and caches.";
      };
      settings = lib.mkOption {
        type = lib.types.attrs;
        default = { };
        description = "Merged into burrow's Renovate settings.";
      };
      path = lib.mkOption {
        type = lib.types.listOf lib.types.package;
        # Node 24 for npm 11: npm 10's arborist crashes resolving deep
        # optional-peer chains (vite 7 -> @vitejs/devtools -> vitest).
        default = with pkgs; [ cargo rustc nodejs_24 pnpm python3 uv go ];
        defaultText = lib.literalExpression "with pkgs; [ cargo rustc nodejs_24 pnpm python3 uv go ]";
        description = "Toolchains Renovate runs to refresh lockfiles.";
      };
    };
  };

  config = lib.mkMerge [
  # Always built and installed, so heartbeat runs the same checks whether or
  # not updates is on (without it, only the ones other modules add).
  {
    burrow.tools.health = healthTool;
    environment.etc = lib.mapAttrs'
      (name: cmd: lib.nameValuePair "burrow/health.d/${name}" {
        source = pkgs.writeShellScript "health-${name}" ''
          export PATH=${checkPath}:/run/wrappers/bin:/run/current-system/sw/bin
          ${cmd}
        '';
      })
      cfg.health;
  }

  (lib.mkIf cfg.enable (lib.mkMerge [
    {
      assertions = [
        {
          assertion = (cfg.claudeManifest == null) == (config.burrow.claude.manifest == null);
          message = "Set burrow.updates.claudeManifest and burrow.claude.manifest together (the same file).";
        }
        {
          assertion = origin.onGithub -> githubRepo != null;
          message = "burrow.updates.origin (${cfg.origin}) is on GitHub, but not as https://github.com/OWNER/NAME, git@github.com:OWNER/NAME or ssh://git@github.com/OWNER/NAME, so which repo it is (and whether its PRs land through a train) can't be read.";
        }
      ];

      # It deploys through the switch, hands problems to proj jobs and tells
      # the owner through notify.
      burrow.switch.enable = true;
      burrow.proj.enable = true;
      burrow.notify.enable = true;

      burrow.updates.health = lib.mapAttrs (_: lib.mkDefault) ({
        nix-daemon = "systemctl -q is-active nix-daemon.socket";
        user-manager = "systemctl -q is-active user@${vars.USER_UID}.service";
        dns = "getent hosts github.com";
        github = "curl -fsS -o /dev/null --max-time 15 https://github.com";
      } // lib.optionalAttrs config.services.openssh.enable {
        sshd = "systemctl -q is-active sshd.service";
      } // lib.optionalAttrs (config.burrow.remoteControl.enable or false) {
        operator = "systemctl --user -M ${user}@ -q is-active claude-rc@${project}.service";
      } // lib.optionalAttrs config.services.tailscale.enable {
        tailscale = "tailscale status --json | jq -e '.BackendState == \"Running\"'";
      } // lib.optionalAttrs config.burrow.events.enable {
        # What starts every handler's pass: down, nothing on GitHub moves anything here.
        events = "systemctl -q is-active burrow-events.service";
      } // lib.optionalAttrs (zfsPools != [ ]) {
        # A degraded mirror keeps running on one disk, quietly, until the other
        # goes. A pool that isn't there fails too, and so does zpool failing,
        # whatever it printed first. A GUID (pool names start with a letter) is
        # whichever imported pool has it, whatever its name.
        zfs = ''
          healthy() {
            local out
            [ -n "$1" ] && out=$(zpool status -x "$1") && grep -qxF "pool '$1' is healthy" <<<"$out"
          }
          named() {
            local pools guid name
            pools=$(zpool list -H -o guid,name) || return 1
            while read -r guid name; do if [ "$guid" = "$1" ]; then echo "$name"; fi; done <<<"$pools"
          }
        '' + lib.concatMapStrings (p: if builtins.match "[0-9]+" p != null
          then "name=$(named ${p}) && healthy \"$name\" || exit 1\n"
          else "healthy ${lib.escapeShellArg p} || exit 1\n") zfsPools;
      } // lib.optionalAttrs (config.burrow.claude.enable or false) {
        # As the user, the way the operator runs it (runuser is root's only).
        claude = ''
          if [ "$(id -un)" = ${user} ]; then claude --version
          else runuser -u ${user} -- env HOME=${account.home} claude --version; fi
        '';
      });

      environment.systemPackages = [ updateTool healthTool ];

      # VM tests run in the Nix build sandbox, as nixbld users; let them at KVM.
      services.udev.extraRules = ''
        KERNEL=="kvm", GROUP="kvm", MODE="0666"
      '';

      systemd.services.${update} = online // {
        description = "Update the system: bump, build, test, switch, confirm";
        # Its own switch must not stop it halfway.
        restartIfChanged = false;
        # Claude Code, for the smoke test after it changes.
        path = [ "/run/current-system/sw" ];
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "${updateTool}/bin/${update}";
          TimeoutStartSec = "4h";
          Nice = 10;
        };
      };
      # A push to the flake's main or a watched input's default branch starts
      # a run within seconds; burrow-events' catch-up starts one when it
      # starts and hourly, for upstreams that send no event.
      burrow.events.handlers.update = {
        events = [ "push" ];
        unit = update;
        perRepo = false;
        repos = cfg.watch;
        pushes = "default";
      };

      systemd.tmpfiles.settings.burrow = {
        ${stateDir}.d = { mode = "0755"; user = "root"; group = "root"; };
        ${config.burrow.scratchDir}.d = { mode = "0755"; inherit user; group = account.group; };
      };

      burrow.claude.machineMd = lib.mkOrder 950 (''
        ## Updates

        ${config.networking.hostName} keeps itself current; nobody runs updates by hand.

        - `${update}` (on each push to main or a watched input, and hourly) bumps every flake input${lib.optionalString (cfg.claudeManifest != null) " and Claude Code"},
          builds, runs the VM tests, switches, checks health, confirms and
          ${if train then "puts it up as a PR from its branch `${update}`, which the merge train lands (while that's open it brings in nothing newer, and rides it on main instead, unless a check fails on it: then a fresh bump replaces its head)" else "pushes to main"}.
          It deploys main too, so a merged PR goes live the same way. It
          holds off while a switch is pending or while ${config.networking.hostName} runs changes that
          aren't on main, so ${if train then "put what you switch to up as a PR: only the merge train moves main" else "commit and push what you switch to"}.
        - `${update} status` says what the last run did. When ${config.burrow.owner} asks how
          updates are going, start there${lib.optionalString (vars.BOOT != "") ", plus `${vars.BOOT} status`"}.
      '' + lib.optionalString rcfg.enable ''
        - Renovate opens and merges dependency PRs in every repo; renovate-sweep
          hands the stuck ones to Claude jobs.
      '' + lib.optionalString cfg.advisories.enable ''
        - advisory-watch hands new CVEs to Claude jobs. Jobs from it show up in
          `proj status`.
      '' + ''
        - Things ${config.burrow.owner} must know go to ${if config.burrow.notify.repo == null then "the journal" else "issues labelled `${config.burrow.notify.label}` in ${config.burrow.notify.repo}"}
          (`${vars.NOTIFY}`).${lib.optionalString cfg.firmware.enable " Flashing firmware stays their call."}
      '');
    }

    (lib.mkIf cfg.advisories.enable {
      environment.systemPackages = [ advisoryWatch pkgs.vulnix pkgs.osv-scanner ];
      systemd.services.advisory-watch = online // {
        description = "Look for new vulnerabilities in what the system and its projects run";
        # A switch leaves a running one be (see burrow.switch's check).
        restartIfChanged = false;
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "${advisoryWatch}/bin/advisory-watch";
          TimeoutStartSec = "1h";
          Nice = 10;
        };
      };
      systemd.timers.advisory-watch = {
        wantedBy = [ "timers.target" ];
        timerConfig = {
          OnBootSec = "20min";
          OnCalendar = "hourly";
          RandomizedDelaySec = "10min";
        };
      };
    })

    (lib.mkIf cfg.firmware.enable {
      environment.systemPackages = [ firmwareCheck ];
      services.fwupd.enable = true;
      systemd.services.firmware-check = online // {
        description = "Look for firmware updates (installing them is the owner's call)";
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "${firmwareCheck}/bin/firmware-check";
        };
      };
      systemd.timers.firmware-check = {
        wantedBy = [ "timers.target" ];
        timerConfig = {
          OnCalendar = "daily";
          Persistent = true;
          RandomizedDelaySec = "1h";
        };
      };
      systemd.services.firmware-review = {
        description = "Monthly look at vendor firmware that LVFS doesn't carry";
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "${firmwareCheck}/bin/firmware-check --review";
        };
      };
      systemd.timers.firmware-review = {
        wantedBy = [ "timers.target" ];
        timerConfig = {
          OnCalendar = "monthly";
          Persistent = true;
        };
      };
    })

    (lib.mkIf rcfg.enable {
      assertions = [{
        assertion = machineApp == null || rcfg.owners != [ ];
        message = "burrow.updates.renovate.owners: name the accounts the App ${machineApp.slug or ""} is installed on, whose repos Renovate looks through.";
      } {
        assertion = renovateEmail == null || !(lib.elem renovateEmail userEmails);
        message = ''
          burrow.updates.renovate.gitAuthor uses ${renovateEmail}, the email the
          user's own commits are made with. Renovate then can't tell a fix pushed
          to its branch from its own commits, and rebases it away. Give Renovate
          an email of its own.'';
      }];
      environment.systemPackages = [ renovateSweep pkgs.renovate ];

      # What moves a repo's Renovate PRs (burrow.events): its default branch
      # (to rebase on, or a dependency changed), and its Renovate PRs being
      # opened, edited or closed, or their CI finishing (Renovate merges a
      # green one on its next run). Each starts Renovate on that repo, and
      # renovate-sweep for its stuck PRs. Both full passes, every repo and the
      # owner's notices, are burrow-events' catch-up, when it starts and
      # hourly: new releases upstream send no event.
      burrow.events.handlers = {
        renovate = {
          events = [ "push" "pull_request" "check_suite" "status" ];
          unit = "renovate";
          pushes = "default";
          branches = [ "renovate/*" ];
        };
        renovate-sweep = {
          events = [ "pull_request" "check_suite" "status" ];
          unit = "renovate-sweep";
          branches = [ "renovate/*" ];
        };
      };

      # Renovate as the machine's own App, or as the user with gh's login,
      # like the other jobs that talk to GitHub: every repo it covers, or
      # (renovate@) the one its instance names. One at a time, however
      # started: they share the clones.
      systemd.services = lib.genAttrs [ "renovate" "renovate@" ] (name: online // {
        description = if name == "renovate" then "Renovate: dependency PRs for every repo" else "Renovate: dependency PRs for %I";
        # A switch leaves a running one be (see burrow.switch's check).
        restartIfChanged = false;
        environment = {
          RENOVATE_CONFIG_FILE = renovateConfig;
          LOG_LEVEL = "info";
        };
        # Renovate's own git, and the toolchains it refreshes lockfiles with.
        path = [ config.programs.git.package pkgs.openssh config.nix.package ] ++ rcfg.path;
        serviceConfig = {
          Type = "oneshot";
          User = user;
          ExecStart = "${renovateRun}/bin/renovate-run${lib.optionalString (name == "renovate@") " %I"}";
          TimeoutStartSec = renovateRunFor;
          Nice = 10;
        };
      }) // lib.genAttrs [ "renovate-sweep" "renovate-sweep@" ] (name: {
        description = "Hand stuck Renovate PRs to a Claude job${lib.optionalString (name == "renovate-sweep@") " (%I)"}";
        # The full one sees what the full Renovate pass left.
        after = lib.optional (name == "renovate-sweep") "renovate.service";
        serviceConfig = {
          Type = "oneshot";
          User = user;
          ExecStart = "${renovateSweep}/bin/renovate-sweep${lib.optionalString (name == "renovate-sweep@") " %I"}";
        };
      });

      systemd.tmpfiles.settings.burrow = {
        ${rcfg.dir}.d = { mode = "0755"; inherit user; group = account.group; };
        "${stateDir}/renovate-sweep".d = { mode = "0755"; inherit user; group = account.group; };
      };
    })

    (lib.mkIf cfg.rustup.enable {
      # Rust toolchains live in ~/.rustup, outside Nix; keep them current.
      systemd.user.services.rustup-update = {
        description = "rustup update";
        unitConfig = {
          ConditionUser = user;
          ConditionPathIsDirectory = "%h/.rustup/toolchains";
        };
        path = [ pkgs.rustup ];
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "${pkgs.rustup}/bin/rustup update";
          Nice = 10;
        };
      };
      systemd.user.timers.rustup-update = {
        wantedBy = [ "timers.target" ];
        unitConfig.ConditionUser = user;
        timerConfig = {
          OnCalendar = "daily";
          Persistent = true;
          RandomizedDelaySec = "1h";
        };
      };
    })
  ]))
  ];
}
