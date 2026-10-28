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
      };
    };
    proj.enable = true;
    remoteControl.enable = true;
    agents.enable = true;
    toolbox.enable = true;
    ops.enable = true;
    robot = {
      enable = true;
      serverIp = "203.0.113.7";
    };
    autoland = {
      enable = true;
      owners = [ "sam" ];
      authors = [ "sam" ];
      reviewers.codex.enable = true;
    };
    house.enable = true;
    browser.enable = true;
    watchdog.enable = true;
    ledger = {
      enable = true;
      # A stand-in: a real machine takes Conary's flake's packages.default.
      package = pkgs.writeShellScriptBin "cn" "exit 1";
      anchorRepo = "sam/homelab";
      daemon = true;
    };
    events = {
      host = "example.org";
      secretFile = "/run/secrets/burrow-webhook";
    };
  };
}
