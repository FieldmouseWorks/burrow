# The smallest machine with every burrow module on: what the `example` check
# builds. A real one adds its disks, network, boot loader and logins.
{ pkgs, ... }:
{
  imports = [ ./machine.nix ];

  burrow = {
    owner = "Sam";
    undo.datasets = [ "rpool/safe/home" "rpool/safe/data" ];
    claude.enable = true;
    undo.enable = true;
    switch = {
      enable = true;
      trialBoot.enable = true;
    };
    notify = {
      enable = true;
      repo = "sam/homelab";
    };
    updates = {
      enable = true;
      origin = "https://github.com/sam/homelab";
      renovate = {
        enable = true;
        autodiscoverFilter = [ "sam/*" ];
        # Dependabot keeps this one's own dependencies.
        followOnly = [ "sam/kept" ];
      };
    };
    proj.enable = true;
    remoteControl.enable = true;
    agents.enable = true;
    toolbox.enable = true;
    rustCache.enable = true;
    ops.enable = true;
    robot = {
      enable = true;
      serverIp = "203.0.113.7";
    };
    heartbeat.enable = true;
    probe = {
      enable = true;
      repo = "sam/homelab";
      authors = [ { login = "sam"; id = 1; } ];
    };
    autoland = {
      enable = true;
      owners = [ "sam" ];
      authors = [ "sam" ];
      app = { id = 123456; accountId = 123457; slug = "sams-steward"; keyFile = "/run/secrets/autoland-app-key"; };
      reviewers.codex.enable = true;
      # Its PRs land through its merge train (on the ledger, below).
      repos."sam/homelab".train = true;
      # Never shown to the reviewers, so never documentation either.
      repos."sam/r".skipPaths = [ "^docs/private/$" ];
    };
    house.enable = true;
    cpu = {
      enable = true;
      gates.slices = [ "ci" ];
    };
    browser.enable = true;
    dashboard = {
      enable = true;
      host = "burrow.tail1234.ts.net";
      owner = { id = 583231; login = "sam"; };
      # A real machine has services.tailscale on and serves it there; the
      # example has no tailnet.
      serve.enable = false;
    };
    # The App the machine's own jobs act as, rather than as sam.
    github = {
      apps.workers = {
        id = 2; accountId = 3;
        slug = "sam-workers";
        keyFile = "/run/secrets/workers-key";
      };
      machinery = "workers";
    };
    watchdog.enable = true;
    watchdog.degradations = {
      enable = true;
      repos."sam/homelab" = [ "flake.yml" ];
    };
    usageReset.enable = true;
    dispatch = {
      enable = true;
      runners.ci-runner = {
        repo = "sam/homelab";
        labels = [ "homelab" ];
      };
    };
    backup = {
      enable = true;
      repository = "rclone:offsite:backups/burrow";
      passwordFile = "/run/secrets/backup-password";
      rcloneConfigFile = "/run/secrets/backup-rclone";
      datasets = [ "rpool/safe/home" "rpool/safe/data" ];
      after = [ "sanoid.service" ];
    };
    ledger = {
      enable = true;
      # A stand-in: a real machine takes Conary's flake's packages.default.
      # It passes the policy file the build checks (`cn policy`).
      package = pkgs.writeShellScriptBin "cn" ''[ "$1" = policy ] && exit 0; exit 1'';
      anchorRepo = "sam/homelab";
      daemon = true;
      # A change to docs alone lands on green CI with no review round.
      policy.docs = [ "^docs/" "(^|/)README\\.md$" ];
      # `undo` issues in the probe's repo, from its authors.
      door.enable = true;
    };
    events = {
      host = "example.org";
      secretFile = "/run/secrets/burrow-webhook";
    };
  };
}
