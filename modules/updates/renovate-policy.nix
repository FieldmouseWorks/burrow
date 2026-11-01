# Renovate's policy for every repo, settings included (a repo's own
# renovate.json can add to it), before ../updates.nix puts lock-age in
# (its renovateSettings).
{ lib, rcfg, githubRepo }:
lib.recursiveUpdate ({
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
  # the subject the steward squashes onto main, and the house check fails a PR
  # whose title isn't. Left to itself Renovate guesses from each repo's
  # history, and config:recommended's preset types it fix or chore in package
  # rules that a run merges after this config's own, so no rule here can
  # outrank them: the preset is ignored instead.
  semanticCommits = "enabled";
  semanticCommitType = "deps";
  semanticCommitScope = null;
  ignorePresets = [ ":semanticPrefixFixDepsChoreOthers" ];
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
  rcfg.settings
