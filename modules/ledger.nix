# ledger: Conary's ledger of every PR on the machine's repos (its `cn`): who
# made each change and why, its revisions, the checks and reviews on them,
# and where it landed. On each PR event for a repo, burrow-events starts that
# repo's sync, which records what's new; the full pass syncs every repo of
# `owners`. It reads GitHub, and writes only the ledger's anchor (a ref in
# `anchorRepo` holding the log's head, so a log that loses ops is refused)
# and each repo's notes (`refs/notes/conary`, a landed change's public-safe
# slice on its trunk commit). `cn why FILE:LINE` in a checkout answers from
# the ledger here, and from the notes in any other clone; `cn verify` checks
# the ledger against the anchor. With `daemon`, conaryd serves the ledger's
# calls on a socket for everything else on the machine: read-only, as a user
# of its own, with no network and no git. Every Claude of burrow.user gets
# them, and `why`, as the `conary` MCP server (conary-mcp).
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.ledger;
  inherit (config.burrow) user;
  inherit (lib) mkOption types;
  autoland = lib.attrByPath [ "burrow" "autoland" ] { } config;
  # Where conaryd listens. Its directory is conaryd's own (RuntimeDirectory).
  socket = "/run/conary/conaryd.sock";

  # The passes' own state: when each repo's next pass is due, for
  # burrow-events ({"OWNER/REPO": epoch}), and the login gh's token is.
  passDir = "${config.burrow.stateDir}/ledger";
  wake = "${passDir}/wake.json";

  sync = pkgs.writeShellApplication {
    name = "burrow-ledger-sync";
    runtimeInputs = [ cfg.package cfg.gh pkgs.coreutils pkgs.jq pkgs.util-linux ];
    text = ''
      # The user's own gh login reads GitHub; its reviews are the steward's.
      # Which login the token is, GitHub is asked once a day per token, not
      # every pass: a call each, and one that fails when the rate limit is
      # spent, before cn could say GitHub is unavailable. The answer is kept
      # under the token's hash, so it's never another token's.
      GH_TOKEN=$(gh auth token)
      login_file=${lib.escapeShellArg "${passDir}/login"}
      key=$(printf %s "$GH_TOKEN" | sha256sum | cut -d' ' -f1)
      # Looked up and refreshed under a lock: passes that overlap (a full
      # one and a repo's) reuse one answer instead of racing to write it.
      exec {login_lock}>>"$login_file.lock"
      flock "$login_lock"
      if read -r was at who <"$login_file" 2>/dev/null && [ "$was" = "$key" ] &&
        [[ $at =~ ^[0-9]+$ ]] && [ $(($(date +%s) - at)) -lt 86400 ] && [ -n "$who" ]; then
        CONARY_REVIEWERS=$who
      else
        CONARY_REVIEWERS=$(gh api user --jq .login)
        (umask 077 && printf '%s %s %s\n' "$key" "$(date +%s)" "$CONARY_REVIEWERS" >"$login_file.new") &&
          mv -f "$login_file.new" "$login_file"
      fi
      exec {login_lock}>&-
      export GH_TOKEN CONARY_REVIEWERS
      export CONARY_LEDGER=${lib.escapeShellArg cfg.dir}
      export CONARY_ANCHOR_REPO=${lib.escapeShellArg cfg.anchorRepo}
      export CONARY_OWN_CONTEXTS=${lib.escapeShellArg (lib.concatStringsSep "," cfg.ownContexts)}
      export CONARY_SESSION_LINKS=${lib.escapeShellArg (lib.concatStringsSep "," cfg.sessionLinks)}
      wake=${lib.escapeShellArg wake}
      # Sets the ledger up, until set-up has finished: its last step keeps
      # the anchor's copy beside the log. Run on a ledger that's set up, it
      # checks the anchor's whole history on GitHub, a call per commit on
      # it, and every sync adds one. A ledger whose key was lost fails here
      # until the key is back, and sync refuses one whose anchor moved.
      [ -e "$CONARY_LEDGER/anchor" ] || cn init >/dev/null

      # GitHub couldn't be asked (its rate limit, its errors, the network;
      # cn exits 75): the repo's pass is booked for five minutes on, and
      # this one hasn't failed. A failed unit is for what retrying won't fix,
      # which is what fails a switch that runs meanwhile. Only a booking
      # written makes it no failure.
      sync_repo() {
        local rc=0
        cn sync "$1" || rc=$?
        [ "$rc" = 75 ] || return "$rc"
        echo "$1: GitHub is unavailable; again in five minutes" >&2
        book "$1" "$(($(date +%s) + 300))"
      }

      # Books a repo's next pass at a time. Every other repo's booking stays,
      # however overdue: burrow-events reads the file only once this pass
      # ends, and takes a booking only if it came after that repo's last
      # pass started, so one already seen to is no pass. Runs as `x || y`
      # does, without errexit, so each step says when it fails.
      book() {
        (
          flock 9 || exit 1
          booked='{}'
          if [ -e "$wake" ]; then
            # Exactly one object: jq reads a stream, so an empty file or
            # two documents would pass a check of each one.
            booked=$(jq -cs 'if length == 1 and (.[0] | type) == "object" then .[0] else error("not one {repo: epoch}") end' "$wake") || exit 1
          fi
          jq -c --arg repo "$1" --argjson at "$2" '. + {($repo): $at}' <<<"$booked" >"$wake.new" || exit 1
          mv -f "$wake.new" "$wake"
        ) 9>>"$wake.lock"
      }

      if [ $# -gt 0 ]; then
        sync_repo "$1"
        exit
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
          sync_repo "$repo" || { echo "couldn't sync $repo" >&2; failed=1; }
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
        readable by burrow.user only. Persist it if / is ephemeral: a ledger
        whose key is lost stops until the key is put back.
      '';
    };

    anchorRepo = mkOption {
      type = types.strMatching "[^/]+/[^/]+";
      example = "sam/homelab";
      description = ''
        The GitHub repository (OWNER/NAME) that keeps the ledger's anchor, at
        `refs/conary/anchor`: one ledger per repository. burrow.user's gh
        login must be able to push to it.
      '';
    };

    owners = mkOption {
      type = types.listOf types.str;
      default = if autoland.enable or false then autoland.owners else [ ];
      defaultText = lib.literalExpression "config.burrow.autoland.owners";
      description = "GitHub users and orgs whose repos the full pass syncs (every non-archived, non-fork repo).";
    };

    gh = mkOption {
      type = types.package;
      default = pkgs.gh;
      defaultText = lib.literalExpression "pkgs.gh";
      internal = true;
      description = "The gh the ledger's passes ask GitHub with (the test's is a stand-in).";
    };

    sessionLinks = mkOption {
      type = types.listOf (types.strMatching "[^/]+/[^/]+");
      default = [ ];
      example = [ "sam/homelab" ];
      description = ''
        Repositories (OWNER/NAME) whose notes may link the agent sessions and
        threads their changes came from. Every other repo's notes leave them
        out, so a repo whose rules keep those links out of its history stays
        clean by default.
      '';
    };

    daemon = lib.mkEnableOption ''
      conaryd, which serves the ledger's calls on ${socket} to everyone on the
      machine, and burrow.user's Claude the `conary` MCP server on it. They
      can already read the op log, so it shows them nothing new. The package
      must have conaryd and conary-mcp (Conary from
      FieldmouseWorks/Conary#19)
    '';

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
    environment.variables = {
      CONARY_LEDGER = cfg.dir;
      CONARY_ANCHOR_REPO = cfg.anchorRepo;
    } // lib.optionalAttrs cfg.daemon { CONARY_SOCKET = socket; };

    systemd.tmpfiles.settings.burrow = {
      ${config.burrow.stateDir}.d = { mode = "0755"; user = "root"; group = "root"; };
      ${cfg.dir}.d = { mode = "0755"; inherit user; group = config.users.users.${user}.group; };
      ${passDir}.d = { mode = "0755"; inherit user; group = config.users.users.${user}.group; };
    };

    burrow.events.handlers.ledger = {
      # A PR opened, pushed, retargeted, closed or merged; a review; CI
      # finishing; a commit status.
      events = [ "pull_request" "pull_request_review" "check_suite" "status" ];
      unit = "burrow-ledger";
      ownStatuses = cfg.ownContexts;
      inherit wake;
    };
    # conary-mcp answers `why` in the session's own checkout, and asks
    # conaryd for the rest. Its own default socket is the user's runtime dir.
    burrow.claude.mcpServers.conary = lib.mkIf cfg.daemon {
      command = "/run/current-system/sw/bin/conary-mcp";
      args = [ "--socket" socket ];
    };
    # Started once the first sync has set the ledger up, and kept running.
    systemd.paths.conaryd = lib.mkIf cfg.daemon {
      wantedBy = [ "paths.target" ];
      pathConfig.PathExists = "${cfg.dir}/key.pub";
    };
    systemd.services = lib.genAttrs [ "burrow-ledger" "burrow-ledger@" ] (name: {
      description = if name == "burrow-ledger"
        then "ledger: record every repo's PRs in Conary's ledger"
        else "ledger: record %I's PRs in Conary's ledger";
      # A switch leaves a running one be (see burrow.switch's check). One
      # that fails while a switch runs fails the switch too, so GitHub being
      # unavailable isn't a failure: it books the repo's next pass (sync_repo).
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
    }) // {
      conaryd = lib.mkIf cfg.daemon {
        description = "ledger: serve Conary's ledger on ${socket}";
        wantedBy = [ "multi-user.target" ];
        unitConfig.ConditionPathExists = "${cfg.dir}/key.pub";
        environment = {
          CONARY_LEDGER = cfg.dir;
          CONARY_SOCKET = socket;
        };
        serviceConfig = {
          ExecStart = "${cfg.package}/bin/conaryd";
          # It only reads the ledger, which everyone may, so it runs as no one
          # in particular, can write nothing but its socket's directory, and
          # reaches nothing but Unix sockets.
          DynamicUser = true;
          RuntimeDirectory = "conary";
          RuntimeDirectoryMode = "0755";
          # Everyone may connect: the socket is as open as the log it reads.
          # conaryd keeps its start lock to itself.
          UMask = "0000";
          ProtectHome = true;
          PrivateNetwork = true;
          RestrictAddressFamilies = [ "AF_UNIX" ];
          CapabilityBoundingSet = "";
          Restart = "always";
          RestartSec = 1;
        };
      };
    };
  };
}
