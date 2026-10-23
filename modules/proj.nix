# proj: the operator hands project work to a Claude inside that project, as a
# background job (proj/proj.sh). Jobs run as burrow.user, in
# burrow.projectsDir/PROJECT, and keep everything they said in jobsDir.
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
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ proj ];
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
  };
}
