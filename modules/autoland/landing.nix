# Who lands each repo's PRs: the steward, or Conary's flow in conaryd
# (Conary#313). The steward, by default, for every repo. A repo listed in
# burrow.landing.conaryd may be given to conaryd with one live setting,
# `settings landing OWNER/REPO conaryd`, and handed back with
# `settings landing OWNER/REPO steward`: no deploy either way, so the way
# back never waits on the pipeline it leaves.
#
# burrow-landing (crates/burrow-landing) moves a repo once the one letting
# go has nothing left at work there, and alone writes the record that says
# who holds it (landing/owners.json) and conaryd's events file, whose `repos` are the
# ones the record gives it. Everything that could land reads the record: the
# steward (its decision), burrow-events (the steward's handlers take only
# its repos' events and wake-ups, and the deliveries of conaryd's go on to
# conaryd), and conaryd (its events file). Its README and docs/landing.md
# say how a handover goes.
inputs:
{ config, lib, pkgs, ... }:
let
  inherit (lib) mkOption types;
  cfg = config.burrow.landing;
  autoland = config.burrow.autoland;
  ledger = config.burrow.ledger;
  inherit (config.burrow) stateDir;
  on = cfg.conaryd != [ ];
  dir = "${stateDir}/landing";
  record = cfg.record;
  events = "${dir}/conaryd-events.json";
  # Counts the events file's publications, written after each one; and
  # the count conaryd last started on.
  serial = "${dir}/conaryd-events.serial";
  started = "${dir}/conaryd-started";
  data = ledger.flow.env.CONARY_DATA or "${ledger.dir}/data";
  # The repos' hooks' secret as burrow-events and the hooks have it,
  # trimmed: conaryd reads a secret file's bytes as they are.
  secret = "/run/conary-flow/webhook";
  socket = "/run/conary-flow/conaryd.sock";
  package = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-landing;
  # conaryd's flow configuration is one file for every repo it's given, so
  # the repos it may have are those whose rendered configurations are the
  # same file (each is a store path of its content).
  files = map autoland.flow.config cfg.conaryd;
  flowConfig = builtins.head files;
  # conaryd's events file, but for the repos burrow-landing gives it, held
  # to Conary's own types as the machine's build loads it (`cn flow
  # settings --events-config`), as the flow configurations are.
  template = pkgs.runCommand "conaryd-events.json" {
    events = builtins.toJSON {
      listen = "127.0.0.1:${toString cfg.port}";
      # The repos' hooks' secret: GitHub signs each delivery with it, and
      # burrow-events passes them on whole.
      secret_path = secret;
      repos = [ ];
      inherit data;
      flow_config = flowConfig;
      policy = ledger.env.CONARY_POLICY;
      token_command = autoland.flow.token;
      anchor_repo = ledger.anchorRepo;
      worker = "${ledger.package}/bin/cn";
    };
    passAsFile = [ "events" ];
  } ''
    HOME=$TMPDIR ${ledger.package}/bin/cn flow settings --events-config "$eventsPath" >/dev/null
    cp "$eventsPath" "$out"
  '';
  driver = pkgs.writeText "burrow-landing.json" (builtins.toJSON {
    settings = config.burrow.settings.file;
    stewardReleased = "${stateDir}/autoland/landing-released.json";
    inherit data;
    # Without conaryd, a repo the record still gives it (its last one taken
    # off at a deploy) is handed back once the workers it left have exited.
    conaryd = if on then { eventsTemplate = cfg.eventsTemplate; inherit events serial started; } else null;
    stewardKick = "${config.burrow.events.kick}/autoland";
  });
in
{
  options.burrow.landing = {
    conaryd = mkOption {
      type = types.listOf (types.strMatching "[^/]+/[^/]+");
      default = [ ];
      example = [ "sam/homelab" ];
      description = ''
        Repos (OWNER/NAME) that may be given to conaryd's landing flow with
        `settings landing OWNER/REPO conaryd`. Being listed gives none of
        them to it: the setting does, and `settings landing OWNER/REPO
        steward` hands one back. Each must be shadowed first
        (burrow.autoland.shadow), so the flow has been checked against the
        steward on it, and they must render one flow configuration, since
        conaryd reads one for every repo it lands. Empty, the default, runs
        no conaryd for landing.
      '';
    };
    record = mkOption {
      type = types.str;
      readOnly = true;
      internal = true;
      default = "${stateDir}/landing/owners.json";
      description = "Who lands each repo now: burrow-landing's record, which every reader acts on.";
    };
    eventsTemplate = mkOption {
      type = types.path;
      readOnly = true;
      internal = true;
      default = template;
      description = "conaryd's events file as rendered, its repos empty: burrow-landing gives it the rest.";
    };
    port = mkOption {
      type = types.port;
      default = 8093;
      description = ''
        Where conaryd's webhook listener binds on 127.0.0.1: burrow-events
        passes it the deliveries of the repos it lands. Read as conaryd
        starts.
      '';
    };
  };

  config = lib.mkIf autoland.enable (lib.mkMerge [
    {
      # Read wherever the steward is, so a repo conaryd held stays its
      # (the steward landing nothing there) even on a machine that stopped
      # running conaryd: nothing lands it twice.
      burrow.events.landing.record = record;
      burrow.events.handlers.autoland.landing = true;
      burrow.events.handlers.autoland-continue.landing = true;
    }
    # burrow-landing wherever the ledger is, conaryd or not: a repo the
    # record gives conaryd is handed back through it even once the last
    # repo is taken off burrow.landing.conaryd.
    (lib.mkIf ledger.enable {
      systemd.tmpfiles.settings.burrow.${dir}.d = {
        mode = "0755";
        user = ledger.flow.user;
        group = config.users.users.${ledger.flow.user}.group;
      };

      systemd.services.burrow-landing = {
        description = "landing: move each repo's landing between the steward and conaryd as its setting says";
        # And once as the machine starts, carrying on a handover a
        # shutdown cut short.
        wantedBy = [ "multi-user.target" ];
        after = [ "burrow-events.service" ];
        # A switch leaves a running one be: the path unit's next start is
        # the new one, and it goes on from the record.
        restartIfChanged = false;
        serviceConfig = {
          Type = "oneshot";
          # The flow's own: it reads conaryd's state, its pass's lock and
          # its workers' locks, and writes conaryd's events file.
          User = ledger.flow.user;
          ExecStart = "${package}/bin/burrow-landing ${driver}";
          # It waits for conaryd's workers in a repo it hands back to
          # exit, each within its job's or proof's own timeout.
          TimeoutStartSec = "${toString (lib.max autoland.jobTimeoutMinutes autoland.proofTimeoutMinutes + 15)}min";
          # A run that fails (a pass of conaryd's that didn't end, a file
          # it couldn't read) goes again from the record.
          Restart = "on-failure";
          RestartSec = "1min";
        };
      };
      # The backstop: a pipeline-only hold's end writes nothing, and a run
      # still waiting on a worker can't hear a change that came meanwhile
      # until it's done (it then goes again on its own).
      systemd.timers.burrow-landing = {
        wantedBy = [ "timers.target" ];
        timerConfig.OnUnitInactiveSec = "5min";
      };
      systemd.paths.burrow-landing = {
        wantedBy = [ "multi-user.target" ];
        pathConfig.PathChanged = [
          config.burrow.settings.effective.overlay
          "${stateDir}/autoland/landing-released.json"
          "/run/current-system"
        ] ++ lib.optional on started;
      };
    })
    (lib.mkIf on {
      assertions = [{
        assertion = ledger.enable;
        message = "burrow.landing.conaryd: conaryd lands through the ledger: enable burrow.ledger.";
      } {
        assertion = lib.all (r: lib.any (s: lib.toLower s == lib.toLower r) autoland.shadow) cfg.conaryd;
        message = "burrow.landing.conaryd: shadow each repo first (burrow.autoland.shadow), so the flow is checked against the steward on it before it can land it.";
      } {
        assertion = lib.all (f: f == flowConfig) files;
        message = "burrow.landing.conaryd: ${lib.concatStringsSep ", " cfg.conaryd} render different flow configurations, and conaryd reads one for every repo it lands.";
      }];

      burrow.events.landing = {
        forward = cfg.port;
        # What Conary's docs/conaryd.md has a repo's hook send it.
        events = [ "pull_request" "pull_request_review" "pull_request_review_comment"
          "pull_request_review_thread" "issue_comment" "check_suite" "check_run" "status" "push" ];
      };

      systemd.services = {
        # The flow's own conaryd, apart from the socket's (which keeps no
        # network and no token): it takes the deliveries burrow-events
        # passes it and lands the repos its events file lists, as the
        # flow's controller where it has one. Started only once
        # burrow-landing has written that file.
        conary-flow = {
          description = "landing: Conary's flow, for the repos burrow-landing gives conaryd";
          wantedBy = [ "multi-user.target" ];
          after = [ "network-online.target" ];
          wants = [ "network-online.target" ];
          unitConfig.ConditionPathExists = [ "${ledger.dir}/key.pub" events ];
          environment = ledger.env // ledger.flow.env // {
            CONARY_SOCKET = socket;
            CONARY_EVENTS_CONFIG = events;
          };
          serviceConfig = {
            User = ledger.flow.user;
            ExecStartPre = pkgs.writeShellScript "conary-flow-secret" ''
              umask 077
              # Trimmed at both ends, as burrow-events and the hooks read it.
              s=$(< "$CREDENTIALS_DIRECTORY/webhook")
              s=''${s#"''${s%%[![:space:]]*}"}
              s=''${s%"''${s##*[![:space:]]}"}
              printf %s "$s" > ${secret}
            '';
            ExecStart = "${ledger.package}/bin/conaryd";
            RuntimeDirectory = builtins.baseNameOf (builtins.dirOf socket);
            RuntimeDirectoryMode = "0700";
            # The repos' hooks' secret and the App's key, for its token.
            LoadCredential = autoland.flow.credentials;
            # Its workers outlive it, as Conary's flow needs: a conaryd
            # that starts waits on the ones still running.
            KillMode = "process";
            Restart = "always";
            RestartSec = 1;
          };
        };
        # A repo given to conaryd gets its first pass from conaryd's
        # start, which kicks every repo it's given. One taken off is let go
        # of only once conaryd has started again without it: a pass reads
        # its repos before it takes the repo's lock, so until then one that
        # read the old file may still start. Then it says which publication
        # it started on, which burrow-landing waits for.
        conary-flow-given = {
          description = "landing: restart conaryd's flow on the repos it's given";
          restartIfChanged = false;
          serviceConfig.Type = "oneshot";
          # The publication it started on, by its serial (written after the
          # file, so conaryd read that one or a later one); again while it
          # changed under it, since a change that comes while this runs may
          # start no run of its own.
          script = ''
            # The serial is `N SHA256`: the count, and the file it counts.
            count() { read -r n _ < ${serial} && echo "$n"; }
            while :; do
              on=$(count)
              ${pkgs.systemd}/bin/systemctl restart conary-flow.service
              printf %s "$on" > ${started}.pending
              chmod 0644 ${started}.pending
              mv ${started}.pending ${started}
              [ "$(count)" = "$on" ] && break
            done
          '';
        };
      };
      systemd.paths = {
        conary-flow-given = {
          wantedBy = [ "multi-user.target" ];
          pathConfig.PathChanged = serial;
        };
      };
    })
  ]);
}
