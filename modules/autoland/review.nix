# The reviewers' service (crates/burrow-review): it takes the looks the
# steward's passes queue (autoland/reviewers.py), as the reviewers' own user,
# and runs each agent in a sandbox that shows it the snapshot and only the
# store paths it needs, its traffic through a gate that lets it reach its
# API and nothing else. Part of burrow.autoland, which imports it.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.autoland;
  rv = cfg.reviewers;
  inherit (import ./paths.nix config) reviewer reviewerHome reviewDir snapDir stewardKick;
  # The reviewers' service's program.
  service = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-review;
  # The review agents (codex).
  agents = import ./agents/package.nix { inherit pkgs; inherit (inputs) nixpkgs-unstable; };
  # What an agent reads the snapshot with, and runs the repo's own checks
  # with (python3: the scripts a repo checks itself with), and bubblewrap
  # for codex's own sandbox inside theirs.
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
  # the gate (burrow-review's), those tools, the links and the machine's CA
  # bundle, with what they need. Never the whole store, which holds every
  # flake's sources this machine has read, private ones included; nor /etc,
  # whose files link into the machine's own configuration.
  reviewStore = pkgs.closureInfo {
    rootPaths = [ agents service config.security.pki.caBundle ]
      ++ reviewTools ++ lib.attrValues reviewLinks;
  };
  # The reviewers' service (crates/burrow-review, config.rs): the queue the
  # passes fill and what it leaves for them, its reviewers, and what a
  # look's sandbox shows.
  reviewConfig = pkgs.writeText "burrow-review.json" (builtins.toJSON {
    settings = config.burrow.settings.effective;
    queue = "${reviewDir}/queue";
    done = "${reviewDir}/done";
    sessions = "${reviewDir}/sessions";
    snapshots = snapDir;
    reviewers.codex = {
      inherit (rv.codex) enable effort parallel;
      off = lib.attrNames (lib.filterAttrs (_: r: !(r.codex or true)) cfg.repos);
    };
    inherit (rv) timeoutMinutes;
    agents = "${agents}/bin";
    # What the agents run commands with: this generation's, which its
    # sandbox's store holds, not the unit's PATH a run inherits.
    path = lib.makeBinPath reviewTools;
    sandbox = "${pkgs.bubblewrap}/bin/bwrap";
    box = {
      store = "${reviewStore}/store-paths";
      caBundle = config.security.pki.caBundle;
      links = reviewLinks;
    };
    inherit (cfg) leadRepos pipelineRepos;
    kick = stewardKick;
  });
  review = pkgs.writeShellScriptBin "burrow-review" ''
    export REVIEW_CONFIG=''${REVIEW_CONFIG:-${reviewConfig}}
    exec ${service}/bin/burrow-review "$@"
  '';
  # The generation deployed now, for a run that outlives a switch (looks
  # the admission policy parks wait in it): the policy it holds looks to,
  # and the program, with that config, it hands the queue to
  # (crates/burrow-review, config.rs `Deployed`). One link in /etc, so
  # they change together, before systemd reloads the unit.
  deployedDir = "burrow/review";
  deployed = pkgs.linkFarm "burrow-review-deployed" [
    { name = "config.json"; path = reviewConfig; }
    { name = "burrow-review"; path = "${review}/bin/burrow-review"; }
  ];

in
{
  imports = [ { key = "burrow/codex-login"; imports = [ (import ../codex-login.nix inputs) ]; } ];
  config = lib.mkIf cfg.enable {

    users.users.${reviewer} = {
      isSystemUser = true;
      group = reviewer;
      home = reviewerHome;
      createHome = true;
    };
    users.groups.${reviewer} = { };

    # The reviews the passes queue, as soon as there are any, as the
    # reviewers' own user: no GitHub login, and the snapshots are all it
    # can read of the repos.
    systemd.services.autoland-review = lib.mkIf (!cfg.dryRun) {
      description = "autoland: the reviewers' reviews of the queued PR heads";
      # A switch leaves a running one be (see burrow.switch's check).
      restartIfChanged = false;
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      environment.HOME = reviewerHome;
      environment.REVIEW_DEPLOYED = "/etc/${deployedDir}";
      serviceConfig = {
        Type = "oneshot";
        User = reviewer;
        ExecStart = "${review}/bin/burrow-review work";
        # However long an agent thinks: no pass gives up on a queued review either.
        TimeoutStartSec = "infinity";
        Nice = 10;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        ReadWritePaths = [ reviewerHome "${reviewDir}/queue" "${reviewDir}/done" "${reviewDir}/sessions" ];
      };
    };
    environment.etc.${deployedDir} = lib.mkIf (!cfg.dryRun) { source = deployed; };
    systemd.paths.autoland-review = lib.mkIf (!cfg.dryRun) {
      wantedBy = [ "paths.target" ];
      # A review to do, or a closed PR's sessions to delete.
      pathConfig = {
        DirectoryNotEmpty = "${reviewDir}/queue";
        PathExistsGlob = "${reviewDir}/sessions/.gone-*";
      };
    };
  };
}
