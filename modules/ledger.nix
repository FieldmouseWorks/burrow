# ledger: Conary's ledger of every PR on the machine's repos (its `cn`): who
# made each change and why, its revisions, the checks and reviews on them,
# and where it landed. On each PR event for a repo, burrow-events starts that
# repo's sync, which records what's new; the full pass syncs every repo of
# `owners`. It only reads GitHub. `cn why FILE:LINE` in a checkout answers
# from it.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.ledger;
  inherit (config.burrow) user;
  inherit (lib) mkOption types;
  autoland = lib.attrByPath [ "burrow" "autoland" ] { } config;

  sync = pkgs.writeShellApplication {
    name = "burrow-ledger-sync";
    runtimeInputs = [ cfg.package pkgs.gh pkgs.coreutils ];
    text = ''
      # The user's own gh login reads GitHub; its reviews are the steward's.
      GH_TOKEN=$(gh auth token)
      CONARY_REVIEWERS=$(gh api user --jq .login)
      export GH_TOKEN CONARY_REVIEWERS
      export CONARY_LEDGER=${lib.escapeShellArg cfg.dir}
      export CONARY_OWN_CONTEXTS=${lib.escapeShellArg (lib.concatStringsSep "," cfg.ownContexts)}
      if [ $# -gt 0 ]; then
        exec cn sync "$1"
      fi
      # One repo or owner failing doesn't hold up the rest, but fails the pass.
      failed=0
      owners=(${lib.escapeShellArgs cfg.owners})
      for owner in "''${owners[@]}"; do
        if ! repos=$(gh repo list "$owner" --limit 1000 --no-archived --source --json nameWithOwner --jq '.[].nameWithOwner'); then
          echo "couldn't list $owner's repos" >&2
          failed=1
          continue
        fi
        while read -r repo; do
          [ -n "$repo" ] || continue
          cn sync "$repo" || { echo "couldn't sync $repo" >&2; failed=1; }
        done <<<"$repos"
      done
      exit "$failed"
    '';
  };
in
{
  imports = [
    (import ./common.nix inputs)
    { key = "burrow/events"; imports = [ (import ./events.nix inputs) ]; }
  ];

  options.burrow.ledger = {
    enable = lib.mkEnableOption "Conary's ledger of every PR, kept up to date from GitHub's events";

    package = mkOption {
      type = types.package;
      description = "Conary, whose `cn` keeps the ledger (its flake's packages.default).";
    };

    dir = mkOption {
      type = types.str;
      default = "${config.burrow.stateDir}/conary";
      defaultText = lib.literalExpression ''"''${config.burrow.stateDir}/conary"'';
      description = ''
        The ledger: its op log, readable by everyone, and its signing key,
        readable by burrow.user only. Persist it if / is ephemeral.
      '';
    };

    owners = mkOption {
      type = types.listOf types.str;
      default = if autoland.enable or false then autoland.owners else [ ];
      defaultText = lib.literalExpression "config.burrow.autoland.owners";
      description = "GitHub users and orgs whose repos the full pass syncs (every non-archived, non-fork repo).";
    };

    ownContexts = mkOption {
      type = types.listOf types.str;
      default = lib.optional (autoland.enable or false) autoland.context;
      defaultText = lib.literalExpression "[ config.burrow.autoland.context ]";
      description = ''
        Commit status contexts that aren't checks: the steward's own status
        is its landing decision, not evidence about the change.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    # `cn` for everyone, on this ledger.
    environment.systemPackages = [ cfg.package ];
    environment.variables.CONARY_LEDGER = cfg.dir;

    systemd.tmpfiles.settings.burrow = {
      ${config.burrow.stateDir}.d = { mode = "0755"; user = "root"; group = "root"; };
      ${cfg.dir}.d = { mode = "0755"; inherit user; group = config.users.users.${user}.group; };
    };

    burrow.events.handlers.ledger = {
      # A PR opened, pushed, retargeted, closed or merged; a review; CI
      # finishing; a commit status.
      events = [ "pull_request" "pull_request_review" "check_suite" "status" ];
      unit = "burrow-ledger";
      ownStatuses = cfg.ownContexts;
    };
    systemd.services = lib.genAttrs [ "burrow-ledger" "burrow-ledger@" ] (name: {
      description = if name == "burrow-ledger"
        then "ledger: record every repo's PRs in Conary's ledger"
        else "ledger: record %I's PRs in Conary's ledger";
      # A switch leaves a running one be (see burrow.switch's check).
      restartIfChanged = false;
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      serviceConfig = {
        Type = "oneshot";
        User = user;
        # Writers take the ledger's lock in turn, so passes queue on it.
        ExecStart = "${sync}/bin/burrow-ledger-sync" + lib.optionalString (name == "burrow-ledger@") " %I";
        TimeoutStartSec = "30min";
        Nice = 10;
      };
    });
  };
}
