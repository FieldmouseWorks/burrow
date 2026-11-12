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
# them, and `why`, as the `conary` MCP server (conary-mcp). With `door`, an
# `undo` issue in a repo (the probe's, by default) asks the ledger for an
# undo with no Claude in between (crates/burrow-ledger-door).
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.ledger;
  inherit (config.burrow) user;
  inherit (config.burrow.github) machineApp;
  inherit (lib) mkOption types;
  autoland = lib.attrByPath [ "burrow" "autoland" ] { } config;
  # What the steward's reviewers aren't shown (their skipPaths, the
  # machine's and each repo's) is the policy's `hidden`, which Conary
  # matches as their snapshot hides a path and never takes for
  # documentation: a change to it gets a review round, whose snapshot names
  # it for the judge. A Conary without `hidden` refuses the file, so the
  # docs shortcut never runs on a matcher that would miss one. A repo's are
  # added to its policy entry by name, as GitHub has it in any case, so a
  # repo is never in the file twice.
  skips = let
    on = autoland.enable or false;
    own = lib.mapAttrs (_: r: lib.filterAttrs (_: v: v != null) r) cfg.policy.repos;
    theirs = lib.filterAttrs (_: s: s != [ ]) (lib.mapAttrs (_: r: r.skipPaths or [ ]) (if on then autoland.repos else { }));
    add = repos: name: paths:
      let hit = lib.findFirst (k: lib.toLower k == lib.toLower name) null (lib.attrNames repos); in
      if hit == null then repos // { ${name} = { hidden = paths; }; }
      else repos // { ${hit} = repos.${hit} // { hidden = paths; }; };
  in {
    machine = if on then autoland.reviewers.skipPaths else [ ];
    repos = lib.foldlAttrs add own theirs;
  };
  # The machine's runners started fresh for one job each, if it has them.
  fresh = lib.attrByPath [ "burrow" "dispatch" ] { } config;
  flow = cfg.flow;
  # CONARY_FLOW_CONFIG: `cn flow`'s machine-owned settings (Conary's
  # docs/flow.md), rendered from what the steward reads, so there is no
  # second copy to keep. Unset plugins are left out: their step waits.
  flowConfig = pkgs.writeText "conary-flow.json" (builtins.toJSON {
    trusted_authors = flow.trustedAuthors;
    settings = {
      inherit (flow) people reviewers;
      review_authors = cfg.reviewers;
    };
    plugins = lib.filterAttrs (_: v: v != null) flow.plugins;
    evidence = {
      signer = cfg.signerWorkflow;
      fresh = if (fresh.enable or false) && fresh.fresh.enable then fresh.fresh.prefix else null;
      inherit (flow.evidence) require negative;
    };
  });
  # Where conaryd listens. Its directory is conaryd's own (RuntimeDirectory).
  socket = "/run/conary/conaryd.sock";

  # The passes' own state: when each repo's next pass is due, for
  # burrow-events ({"OWNER/REPO": epoch}).
  passDir = "${config.burrow.stateDir}/ledger";
  wake = "${passDir}/wake.json";
  doorWake = "${passDir}/door-wake.json";

  # How a writer of the ledger asks GitHub, and what it tells cn: the
  # passes' and the door's alike.
  writer = (if machineApp == null then ''
    # The user's own gh login reads GitHub and writes the anchor, notes and
    # undo PRs.
    GH_TOKEN=$(gh auth token)
    export GH_TOKEN
  '' else ''
    # The machine's own App reads GitHub and writes the anchor, notes and
    # undo PRs: each repo's token from its owner's installation, asked for
    # each repo.
    # shellcheck source=/dev/null
    . ${machineApp.envFile}
    export CONARY_TOKEN_COMMAND=${lib.escapeShellArg "${config.burrow.tools.gh-token}/bin/gh-token get"}
  '') + ''
    # What cn is told about the ledger (env, below).
    ${lib.concatStringsSep "\n" (lib.mapAttrsToList (k: v: "export ${k}=${lib.escapeShellArg v}") cfg.env)}
  '';

  # cn, as a writer of the ledger: the door's, and the steward's undos (writer).
  writerCn = pkgs.writeShellApplication {
    name = "burrow-ledger-cn";
    runtimeInputs = [ cfg.package cfg.gh ];
    text = writer + ''
      exec cn "$@"
    '';
  };

  # What crates/burrow-ledger-door reads (its Config).
  doorConfig = pkgs.writeText "burrow-ledger-door.json" (builtins.toJSON {
    inherit (cfg.door) repo label authors;
    host = config.networking.hostName;
    cn = lib.getExe cfg.writer;
  });

  sync = pkgs.writeShellApplication {
    name = "burrow-ledger-sync";
    runtimeInputs = [ cfg.package cfg.gh pkgs.coreutils pkgs.jq pkgs.util-linux ];
    text = writer + ''
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
    { key = "burrow/github"; imports = [ (import ./github.nix inputs) ]; }
  ];

  options.burrow.ledger = {
    enable = lib.mkEnableOption "Conary's ledger of every PR, kept up to date from GitHub's events";

    package = mkOption {
      type = types.package;
      description = ''
        Conary, whose `cn` keeps the ledger (its flake's packages.default).
        The package must carry its runtime tools, including git; units do
        not supply them.
      '';
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
        `refs/conary/anchor`: one ledger per repository. The machine's own App
        (burrow.github.machinery), else burrow.user's gh login, must be able
        to push to it.
      '';
    };

    signerWorkflow = mkOption {
      type = types.strMatching "[^/]+/[^/]+/\\.github/workflows/[^/@]+";
      default = "FieldmouseWorks/burrow/.github/workflows/flake-check.yml";
      description = ''
        The one workflow whose jobs' attestations the train counts, at a
        commit on its repo's default branch, as OWNER/NAME/PATH (Conary's
        docs/ci-attestation.md, rule 2): burrow's shared CI, which every
        project's gate calls.
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
      default = config.burrow.github.machineGh;
      defaultText = lib.literalExpression "config.burrow.github.machineGh";
      internal = true;
      description = "The gh the ledger's passes ask GitHub with (the test's is a stand-in).";
    };

    policy = mkOption {
      type = types.submodule {
        options = let
          paths = what: mkOption {
            type = types.listOf types.str;
            default = [ ];
            description = "Path regexes ${what}.";
          };
          rules = {
            r2 = paths "that make a change R2: it rides alone, and a Claude job judges it before it lands";
            docs = paths "naming documentation: a change to these alone needs green CI and no review round, unless it's R2";
            notDocs = paths "that are never documentation, whatever `docs` says: a design doc, a page the repo publishes. AGENTS.md, CLAUDE.md and `.github/` never are";
            live = mkOption {
              type = types.nullOr types.bool;
              default = null;
              description = "Whether landing goes live (R1) rather than changing nothing outside the repo (R0); unset, the machine's.";
            };
          };
        in rules // {
          r2 = rules.r2 // {
            default = [ "(^|/)CODEOWNERS$" "(^|/)secrets?/" "(^|/)\\.sops\\.yaml$" "\\.sops\\.(ya?ml|json|env)$" ];
          };
          repos = mkOption {
            type = types.attrsOf (types.submodule { options = rules; });
            default = { };
            example = lib.literalExpression ''
              { "sam/app" = { r2 = [ "^migrations/" ]; docs = [ "^docs/" ]; live = true; }; }
            '';
            description = "Each repo's (OWNER/NAME) additions: its lists add to the machine's, so no repo takes a path off R2.";
          };
        };
      };
      default = { };
      description = ''
        Conary's class policy (its docs/DESIGN.md, section 4.4): which
        changes are R2, which are documentation only, and which repos go
        live when they land, from the paths a change touches, renames and
        copies included. `cn class` reads it (CONARY_POLICY), and the
        steward asks it for every PR it settles. The steward's reviewers'
        skipPaths are never documentation: they go to Conary as `hidden`. The machine's R2 paths
        default to CODEOWNERS (who signs off on what) and secrets. The rest
        of `.github/` isn't on it: the reviewers read workflow changes like
        any other, and the steward merges them once settled. Nor are
        database migrations: what undoes a bad one is the backup its deploy
        takes before migrating, not a wait at merge. A repo whose deploy
        takes no such backup puts `(^|/)migrations?/` on its own list.
      '';
    };

    flow = {
      enable = lib.mkEnableOption ''
        CONARY_FLOW_CONFIG (`env`, and every login): the config `cn flow`
        reads, rendered from the settings the steward reads. Nobody writes it
        by hand: defining that variable elsewhere fails the build, since it
        would be a second copy of the steward's settings. Conary still reads
        a plain file, so it runs without burrow
      '';
      trustedAuthors = mkOption {
        type = types.listOf types.str;
        default = autoland.authors or [ ];
        defaultText = lib.literalExpression "config.burrow.autoland.authors";
        description = "Whose PRs merge themselves once settled: the steward's authors.";
      };
      people = mkOption {
        type = types.listOf types.str;
        default = autoland.authors or [ ];
        defaultText = lib.literalExpression "config.burrow.autoland.authors";
        description = "GitHub users whose approval an R2 change or an owner decision waits for.";
      };
      reviewers = mkOption {
        type = types.listOf types.str;
        default = [ "reviewer" ];
        description = "The reviewer roles flow counts (`settings.reviewers`).";
      };
      plugins = lib.genAttrs [ "review" "fix" "judge" "proof" ] (kind: mkOption {
        type = types.nullOr types.path;
        default = null;
        description = "Absolute path of the ${kind} plugin executable (Conary's docs/plugins.md); unset, that step waits.";
      });
      evidence = {
        require = mkOption {
          type = types.listOf types.str;
          default = [ "check" ];
          description = "Checks that count as green, each attested by `signerWorkflow`.";
        };
        negative = mkOption {
          type = types.str;
          default = "attest / negative";
          description = "The check that attests the new tests fail on old code.";
        };
      };
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

    reviewers = mkOption {
      type = types.listOf (types.strMatching ".+[[]bot]");
      default = lib.optional (autoland.enable or false) "${autoland.app.slug}[bot]";
      defaultText = lib.literalExpression ''[ "''${config.burrow.autoland.app.slug}[bot]" ]'';
      description = ''
        The steward's logins, as GitHub's REST API gives them: only their
        statuses are its landing decision, and only their reviews are its
        reviewers'. A GitHub App's bot each, since a user's login can't
        end in "[bot]" and a status from anyone else's mustn't count, the
        owner's own included.
      '';
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

    env = mkOption {
      type = types.attrsOf types.str;
      readOnly = true;
      default = {
        CONARY_LEDGER = cfg.dir;
        CONARY_ANCHOR_REPO = cfg.anchorRepo;
        # The steward's word (its status, its reviewers' reviews) is only
        # what its App posted.
        CONARY_REVIEWERS = lib.concatStringsSep "," cfg.reviewers;
        CONARY_OWN_CONTEXTS = lib.concatStringsSep "," cfg.ownContexts;
        CONARY_SESSION_LINKS = lib.concatStringsSep "," cfg.sessionLinks;
        # Whose attestations count: a train's gate and a PR's negative.
        CONARY_SIGNER_WORKFLOW = cfg.signerWorkflow;
        # null where unset, which Conary takes as the machine's. Checked by
        # the machine's own cn as it's built (`cn policy`): a cn that doesn't
        # know a key of it (an older Conary than this burrow needs) fails the
        # build, rather than every class the steward asks once it's live,
        # which would stop it landing the bump that fixes it.
        CONARY_POLICY = "${pkgs.runCommand "conary-policy.json" {
          policy = builtins.toJSON {
            inherit (cfg.policy) r2 docs live notDocs;
            hidden = skips.machine;
            repos = skips.repos;
          };
          passAsFile = [ "policy" ];
        } ''
          ${cfg.package}/bin/cn policy "$policyPath" >/dev/null
          cp "$policyPath" "$out"
        ''}";
      } // lib.optionalAttrs flow.enable {
        CONARY_FLOW_CONFIG = "${flowConfig}";
      } // lib.optionalAttrs ((fresh.enable or false) && fresh.fresh.enable) {
        # A self-hosted runner's CI counts only from one started fresh for
        # its job, and only burrow.dispatch.fresh names a runner so
        # (Conary's docs/ci-attestation.md): one that took a job before
        # could have signed in this one's name.
        CONARY_FRESH_RUNNERS = fresh.fresh.prefix;
      };
      defaultText = lib.literalExpression "{ CONARY_LEDGER = dir; CONARY_ANCHOR_REPO = anchorRepo; ... }";
      description = ''
        What every writer of the ledger tells `cn` about it: its passes, and
        the steward's merge train (burrow.autoland's `train`), each with its
        own CONARY_TOKEN_COMMAND, and the workflow whose attestations count.
        On a machine with runners started fresh for one job each
        (burrow.dispatch.fresh), the start of their names.
      '';
    };

    door = {
      enable = lib.mkEnableOption ''
        the undo door: an issue labelled `label` in `repo`, from one of
        `authors`, asks for one `cn undo` (a PR, a session, or since a time),
        which runs as the ledger's writers do and answers with the undo's PR,
        with no Claude in between. The PR is the steward's to settle like any
        other, and an undo can itself be undone
      '';
      repo = mkOption {
        type = types.nullOr (types.strMatching "[^/]+/[^/]+");
        default = config.burrow.probe.repo or null;
        defaultText = lib.literalExpression "config.burrow.probe.repo";
        description = "OWNER/NAME of the repo whose issues ask for undos: the probe's, by default.";
      };
      label = mkOption {
        type = types.str;
        default = "undo";
        description = "The label that makes an issue an undo request.";
      };
      authors = mkOption {
        type = lib.types.listOf (import ../lib/github-author.nix { inherit lib; });
        default = config.burrow.probe.authors or [ ];
        defaultText = lib.literalExpression "config.burrow.probe.authors";
        description = "Who may ask for an undo (GitHub accounts and numeric IDs): the probe's, by default.";
      };
    };

    writer = mkOption {
      type = types.package;
      readOnly = true;
      default = writerCn;
      defaultText = lib.literalMD "`cn` as the ledger's writers run it";
      description = ''
        `cn` as a writer of the ledger (burrow-ledger-cn): as the machine's
        App where there is one, else the user's gh login, told about the
        ledger as `env` says. The undo door runs its undos with it, and so
        does the steward a red default branch's (autoland/repos.py), so an
        undo's PR is the machinery's, like the fixes its jobs open.
      '';
    };

    trainContext = mkOption {
      type = types.str;
      readOnly = true;
      default = "conary/train";
      description = ''
        The commit status `cn train` posts, as the steward's App, on the
        head of each PR it lands or turns away (Conary's docs/DESIGN.md,
        section 4.7), which a train repo's rules require: its word on the
        PR, not a check of it.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [{
      assertion = cfg.reviewers != [ ];
      message = "burrow.ledger.reviewers: name the steward's App (its SLUG[bot]), or no merge could be told from a bypass.";
    } {
      assertion = cfg.door.enable -> cfg.door.authors != [ ];
      message = "burrow.ledger.door.authors: name who may ask for an undo.";
    } {
      assertion = cfg.door.enable -> cfg.door.repo != null;
      message = "burrow.ledger.door.repo: name the repo whose issues ask for undos.";
    } {
      assertion = !flow.enable || (flow.trustedAuthors != [ ] && flow.people != [ ] && flow.evidence.require != [ ]);
      message = "burrow.ledger.flow: name trustedAuthors, people and evidence.require (the first two default to the steward's authors).";
    }];

    # `cn` for everyone, on this ledger.
    environment.systemPackages = [ cfg.package ];
    environment.variables = {
      CONARY_LEDGER = cfg.dir;
      CONARY_ANCHOR_REPO = cfg.anchorRepo;
    } // lib.optionalAttrs cfg.daemon { CONARY_SOCKET = socket; }
      // lib.optionalAttrs flow.enable { CONARY_FLOW_CONFIG = "${flowConfig}"; };

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
      # The train's status is no evidence either: cn never records it.
      ownStatuses = cfg.ownContexts ++ [ cfg.trainContext ];
      inherit wake;
    };
    # Only the door repo's issue events: a comment asks nothing (reopen to
    # ask again), and a pass reads every open request anyway.
    burrow.events.handlers.ledger-door = lib.mkIf cfg.door.enable {
      events = [ "issues" ];
      unit = "burrow-ledger-door";
      perRepo = false;
      repos = [ cfg.door.repo ];
      wake = doorWake;
      gap = 2;
    };
    burrow.claude.machineMd = lib.mkIf cfg.door.enable (lib.mkOrder 703 ''
      An undo needs no Claude either: open an issue in ${cfg.door.repo} labelled
      `${cfg.door.label}` (as ${lib.concatStringsSep " or " (map (a: a.login) cfg.door.authors)}), its body one line:
      `undo OWNER/REPO#N`, `undo --actor SESSION_URL` or
      `undo --since RFC3339_TIME --repo OWNER/REPO`. The undo's PR comes back as
      a comment, and the steward settles it like any other.
    '');
    # conary-mcp answers `why` in the session's own checkout, and asks
    # conaryd for the rest. Its own default socket is the user's runtime dir.
    burrow.mcpServers.conary = lib.mkIf cfg.daemon {
      command = "/run/current-system/sw/bin/conary-mcp";
      args = [ "--socket" socket ];
      readPaths = [ (builtins.dirOf socket) ];
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
      burrow-ledger-door = lib.mkIf cfg.door.enable {
        description = "ledger: answer the undo issues in ${cfg.door.repo}";
        # A switch leaves a running pass be (see burrow.switch's check); the
        # next one is the new system's.
        restartIfChanged = false;
        after = [ "network-online.target" ];
        wants = [ "network-online.target" ];
        path = [ cfg.gh ];
        environment.GH_NO_UPDATE_NOTIFIER = "1";
        environment.BURROW_DOOR_WAKE = doorWake;
        serviceConfig = {
          Type = "oneshot";
          # burrow.user, whose key signs the ledger's ops; cn takes the
          # ledger's lock, so it queues behind a pass.
          User = user;
          ExecStart = "${lib.getExe inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-ledger-door} ${doorConfig}";
          TimeoutStartSec = "30min";
        };
      };
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
