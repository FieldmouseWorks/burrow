# proj's limits, for real: a machine with proj on and a stand-in for Claude
# (burrow.claude.package) checks that jobs past maxJobs wait in line, that
# --first goes ahead but can't starve a turn that has waited long, that a
# pipeline job's every turn, in one of the pipeline's own repos, takes the
# slot kept for them when the rest are full, and one in a lead repo goes
# ahead of the others, while one anywhere else goes as --first and says so
# (pick.sh's own check has how origins are told apart),
# that a job whose turn has ended frees its slot, that one stopped after
# its answer with work in the background has the rest of that run stopped,
# hears so once and answers again (the turn ending as its last run did, its
# log saying so even when its unit went first, and within one timeout that
# the clearing up between runs counts against), that one waiting on a PR or
# issue (proj await) is resumed when it changes, even before the turn ended and
# from proj-watch's own unit, but never for proj-inbox's own comment, or once
# its deadline passes with GitHub unreadable, that a reply meanwhile owns the
# job's waits, that a turn's end kicks what it was started to and proj-watch
# for the repos it awaits, that a job's own --timeout stops its turns, and
# its --idle one that has gone quiet (and either still kicks), that a reply
# while the last turn's unit is still ending waits for it before its turn is
# written, that _watch and proj-inbox given a repo look at
# that repo alone and book when it next needs a look, that proj-inbox posts
# every finished turn's answer (one
# resumed before it looked too, none misread while a turn starts) and ends
# the waits of one whose issue closed (proj stop's, which a watch already
# asking GitHub keeps to), a stop that fails there not holding up the rest,
# that a job dir removed or a wait it can't read stops neither proj-inbox nor
# _watch, that every turn says what comes next (proj await, ask or done), one
# that doesn't hearing so once and, still silent, being the stall watchdog's
# at once (as one that ended in a failure is, a turn in line or running to its
# deadline, and an await to its own, the watchdog kicked as they change),
# while a question, a finished job, a stopped one and an --owned job's end
# aren't, that a job run --app acts as its App every turn, whatever the login
# sets and if the App goes while it waits, or not at all, and a bot's issue's
# job as the machinery's App, and
# that a job past its memory cap is killed by its own cgroup's limit rather
# than the machine running out, and that a turn killed before it answers can
# still be continued, in its own session.
#
#   nix build .#checks.x86_64-linux.proj -L
{ self, pkgs }:
let
  inherit (import ./proj/stand-ins.nix { inherit pkgs; }) claude gh;
  # autoland's lists of the pipeline's own repos and its lead ones, which
  # proj reads, without the rest of autoland: web is one of the first here,
  # and lead the only one of the second.
  pipelineRepos = {
    options.burrow.autoland = {
      pipelineRepos = pkgs.lib.mkOption { type = pkgs.lib.types.listOf pkgs.lib.types.str; };
      leadRepos = pkgs.lib.mkOption { type = pkgs.lib.types.listOf pkgs.lib.types.str; };
    };
    config.burrow.autoland = {
      pipelineRepos = [ "O/Web" ];
      leadRepos = [ "o/Lead" ];
    };
  };
in
pkgs.testers.runNixOSTest {
  name = "proj";

  nodes.machine = {
    imports = [
      self.nixosModules.proj
      self.nixosModules.watchdog
      pipelineRepos
    ];
    burrow = {
      claude.package = claude;
      # On, so proj keeps its waits and kicks it; it never runs here (no
      # burrow-events), the test reads what proj writes.
      watchdog.enable = true;
      proj = {
        enable = true;
        # Two slots for anyone, one more for pipeline jobs alone, and no cap
        # that binds before the slots run out (pick.sh's own check has those).
        maxJobs = 3;
        pipelineSlots = 1;
        maxJobsPerProject = 2;
        # The VM is small: only the slot count holds jobs back here.
        minFreeMemory = 0;
        jobMemoryMax = "128M";
        # Well past what the --first subtest takes from queueing its plain
        # job to freeing a slot for it (two runs, a pause and a stop, each
        # telling the watchdog; over 20s on a loaded host), so that job
        # never goes ahead for having waited.
        maxWait = 45;
        inherit gh;
        inbox = {
          enable = true;
          authors = [ "sam" "sam-steward[bot]" ];
        };
      };
      # The App the machine's own work acts as. Its socket isn't asked here:
      # what's checked is that a job runs in its env.
      github = {
        apps.workers = {
          id = 7;
          slug = "sam-workers";
          keyFile = "/run/keys/workers.pem";
          users = [ "dev" ];
        };
        machinery = "workers";
      };
    };
    # proj-watch and proj-inbox run by hand here, never from burrow-events,
    # whose kick port the test listens on itself.
    systemd.services.burrow-events.enable = false;
    systemd.timers.burrow-events-catch-up.enable = false;
    environment.systemPackages = [
      pkgs.git
      pkgs.python3
    ];

    users.users.dev = {
      isNormalUser = true;
      uid = 1001;
      group = "dev";
    };
    users.groups.dev.gid = 1001;
    systemd.tmpfiles.rules = [
      "d /data/dev/src/web 0755 dev dev -"
      "d /data/dev/src/lead 0755 dev dev -"
      "d /data/dev/src/ib 0755 dev dev -"
      "d /data/dev/src/bt 0755 dev dev -"
    ];
    virtualisation.memorySize = 2048;
    # The test framework panics on any OOM, a cgroup's own included; a real
    # machine kills inside the cgroup, which is what's checked here.
    boot.kernel.sysctl."vm.panic_on_oom" = 0;
  };

  testScript = builtins.readFile ./proj/test.py;
}
