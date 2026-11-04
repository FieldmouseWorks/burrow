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

  # What renovate-sweep puts on a lock file refresh to have it redone
  # (modules/autoland/lock_wait.py's REQUEST): Renovate redoes the branch on
  # its next run and takes the label off, so one ask is one redo. A
  # "rebase!" title has it redo the branch on every run until someone takes
  # it off, and each of those pushes starts the next run.
  rebaseLabel = "rebase";

  # The rules below take a lock file refresh, and a digest update on
  # GitHub, without a release timestamp on purpose, and each time Renovate
  # warns that it did. A warning lands on the dashboard's Repository
  # Problems, where nothing can clear it and the steward takes it for
  # something stuck. Only the warning for an upgrade that went ahead: any
  # other still counts.
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
      # A flake input pinned by commit to a branch of a GitHub repo
      # (`?ref=main&rev=...`) is looked up the way an action pinned to a
      # branch is: on GitHub, where the branch's head has a commit date, so
      # the soak (or, for trustedOwners' repos, the batching below) ages it
      # and groups it with the pins on the same repo. Through git it has
      # none: its update waits on a timestamp that never comes, and the
      # lookup never learns its source URL. The nix manager gives an input
      # a branch to follow (currentValue) only when it's pinned by commit,
      # but nixpkgs one always, with the nixpkgs versioning that offers it
      # a newer channel: nixpkgs keeps it. An input with no rev moves only
      # with the lock file refresh.
      matchManagers = [ "nix" ];
      matchDatasources = [ "git-refs" ];
      matchPackageNames = [ "https://github.com/*/*" ];
      matchJsonata = [ "$exists(currentValue) and versioning != 'nixpkgs'" ];
      overrideDatasource = "github-digest";
      overridePackageName = "{{{replace '\\.git$' '' (replace '^[^:]+://[^/]+/' '' packageName)}}}";
      versioning = "exact";
    }
    {
      # Renovate ages a digest update on GitHub (a branch pin's new commit,
      # or a commit pin put on a branch) when it looks it up, against the
      # branch head's commit date, and holds it there (internalChecksFilter)
      # until it's old enough. It doesn't hand that date on to the update,
      # so the branch's own stability check finds none and, by default,
      # stays pending for good: the PR never merges. The lookup has already
      # held it, so the branch's check goes ahead without a date.
      matchDatasources = [ "github-digest" ];
      matchUpdateTypes = [ "digest" "pinDigest" ];
      minimumReleaseAgeBehaviour = "timestamp-optional";
    }
  ] ++ lib.optionals (rcfg.trustedOwners != [ ]) [
    {
      # A dependency on a repo of trustedOwners' (a pinned workflow, an
      # action, a flake input) waits for no soak: the soak is against
      # someone else's hijacked release, and on a repo that's still moving
      # it never ends. Each new commit restarts the three days, so a pin on
      # burrow's main sat in "pending status checks" while burrow merged
      # daily. Only where the source URL is the repo the datasource fetches
      # from (GitHub's own, git's): a registry's package (a Docker image, an
      # npm package) claims whatever source URL it likes, and an OWNER/NAME
      # on another host is someone else's.
      matchDatasources = [ "github-tags" "github-digest" "github-releases" "git-refs" "git-tags" ];
      matchSourceUrls = map (o: "https://github.com/${o}/*") rcfg.trustedOwners;
      minimumReleaseAge = null;
    }
    {
      # Everything pinned to a branch of one of their repos (its workflows,
      # its actions, the flake input) moves in one PR, once that branch has
      # been quiet for half an hour. It's batching, not a soak: a burst of
      # merges upstream comes down as one PR, not one a merge, each running
      # the project's whole gate. A branch that goes on moving holds it
      # only until it stops for half an hour, and Renovate's next run on
      # the project (its own events, or burrow-events' hourly catch-up)
      # opens it. A PR already open keeps its commit meanwhile.
      matchDatasources = [ "github-digest" ];
      matchSourceUrls = map (o: "https://github.com/${o}/*") rcfg.trustedOwners;
      minimumReleaseAge = "30 minutes";
      groupName = "{{{sourceRepo}}}";
      groupSlug = "{{{sourceRepoSlug}}}";
    }
  ] ++ lib.optional (githubRepo != null) {
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
