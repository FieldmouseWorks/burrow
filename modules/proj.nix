# proj: the operator hands project work to a Claude inside that project, as a
# background job (proj/proj.sh). Jobs run as burrow.user, in
# burrow.projectsDir/PROJECT, and keep everything they said in jobsDir.
# With proj.inbox, issues labelled for Claude in the projects' GitHub repos
# become jobs too (proj/inbox.sh).
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.proj;
  inherit (config.burrow) user projectsDir;

  proj = pkgs.writeShellApplication {
    name = "proj";
    runtimeInputs = [ pkgs.coreutils pkgs.findutils pkgs.gnugrep pkgs.jq pkgs.systemd pkgs.bashInteractive ];
    text = ''
      SRC=${lib.escapeShellArg projectsDir}
      JOBS=${lib.escapeShellArg cfg.jobsDir}
    '' + builtins.readFile ./proj/proj.sh;
  };

  inbox = pkgs.writeShellApplication {
    name = "proj-inbox";
    runtimeInputs = [ proj pkgs.coreutils pkgs.git pkgs.gh pkgs.jq pkgs.gnused pkgs.gnugrep ];
    text = ''
      SRC=${lib.escapeShellArg projectsDir}
      JOBS=${lib.escapeShellArg cfg.jobsDir}
      STATE=${lib.escapeShellArg "${config.burrow.stateDir}/proj-inbox"}
      SCRATCH=${lib.escapeShellArg config.burrow.scratchDir}
      LABEL=${lib.escapeShellArg cfg.inbox.label}
      AUTHORS=${lib.escapeShellArg (lib.concatStringsSep " " cfg.inbox.authors)}
      VM=${if cfg.inbox.vm then "1" else "0"}
      HOST=${lib.escapeShellArg config.networking.hostName}
      NOTIFY=${lib.escapeShellArg (lib.optionalString (config.burrow.tools ? notify) (lib.getExe config.burrow.tools.notify))}
    '' + builtins.readFile ./proj/inbox.sh;
  };
in
{
  imports = [ (import ./common.nix inputs) ];

  options.burrow.proj = {
    enable = lib.mkEnableOption "proj, background Claude jobs in each project";
    jobsDir = lib.mkOption {
      type = lib.types.str;
      default = "/data/dev/jobs";
      description = "Where each job's prompts, transcript and answer are kept.";
    };

    inbox = {
      enable = lib.mkEnableOption ''
        proj-inbox: an open issue labelled `inbox.label` in a project's GitHub repo,
        opened by one of `inbox.authors`, becomes a proj job in that project; its
        answers go back as comments and their replies there continue it
      '';
      label = lib.mkOption {
        type = lib.types.str;
        default = "claude";
        description = "The label that hands an issue to Claude. It's created in each repo that lacks it.";
      };
      authors = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        example = [ "octocat" ];
        description = ''
          GitHub logins whose issues and replies Claude acts on. Anyone else's
          are left alone, labelled or not: an issue is a task given to an agent
          with your logins.
        '';
      };
      vm = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = "Run issue jobs in agent sandboxes (proj run --vm) instead of on the machine.";
      };
      every = lib.mkOption {
        type = lib.types.str;
        default = "*:0/3";
        description = "How often the inbox looks at the repos (a systemd OnCalendar).";
      };
    };
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [ {
    # Claude Code too, for when burrow.claude (which also installs it) is off.
    environment.systemPackages = [ proj config.burrow.claude.package ];
    burrow.tools.proj = proj;
    # Jobs are units in the user's systemd manager, so they outlive the session
    # that started them.
    users.users.${user}.linger = lib.mkDefault true;
    systemd.tmpfiles.rules = [ "d ${cfg.jobsDir} 0755 ${user} ${config.users.users.${user}.group} -" ];

    burrow.claude.machineMd = lib.mkOrder 700 ''
      ## Projects, from the operator

      ${config.burrow.owner} talks to one chat, the operator, for machine and project work alike. For
      project work, hand it to a Claude inside that project, so it gets the
      project's own CLAUDE.md and history:

          proj list                         projects in ${projectsDir}
          proj run web "the task"           start a job; prints its id
          proj status                       what's running, what finished
          proj log ID                       its answer (--raw for everything)
          proj reply ID "follow-up"         continue the same conversation
          proj stop ID

      Jobs run in the background and survive this session ending. Check on them,
      relay results to ${config.burrow.owner}, and follow up yourself where the answer is obvious.
      A job can't ask ${config.burrow.owner} anything: R2 commands get refused, so bring those to
      them. ${config.burrow.owner} can also open any project's own session directly from their phone.
    '';
  }

  (lib.mkIf cfg.inbox.enable {
    assertions = [{
      assertion = cfg.inbox.authors != [ ];
      message = "burrow.proj.inbox.authors: name who may hand Claude issues.";
    }];
    environment.systemPackages = [ inbox ];
    systemd.services.proj-inbox = {
      description = "Hand GitHub issues labelled ${cfg.inbox.label} to proj jobs";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      serviceConfig = {
        Type = "oneshot";
        User = user;
        ExecStart = "${inbox}/bin/proj-inbox";
      };
    };
    systemd.timers.proj-inbox = {
      wantedBy = [ "timers.target" ];
      timerConfig.OnCalendar = cfg.inbox.every;
    };
    systemd.tmpfiles.rules = [ "d ${config.burrow.stateDir}/proj-inbox 0755 ${user} ${config.users.users.${user}.group} -" ];

    burrow.claude.machineMd = lib.mkOrder 701 ''
      Issues work too: label one `${cfg.inbox.label}` in a project's GitHub repo
      (opened by ${lib.concatStringsSep " or " cfg.inbox.authors}) and within minutes it's a job there.
      The answer comes back as a comment, a reply on the issue continues the job,
      and closing it or taking the label off ends it. `proj-inbox status` shows
      which issue is which job.
    '';
  })
  ]);
}
