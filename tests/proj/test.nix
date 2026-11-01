# The machine proj's VM tests run on, and what their scripts share: proj on
# with stand-ins for Claude (burrow.claude.package) and gh, three slots, the
# inbox and the workers App, and helpers to run proj as its user and read
# what it writes. Each test is a name and its subtests:
#
#   import ./proj/test.nix { inherit self pkgs; } "proj" ''with subtest(...): ...''
#
# proj.nix has a job's turns, proj-second-runs.nix a turn's second run once
# its first is stopped, proj-slots.nix who waits in line and who goes first,
# and proj-github.nix its GitHub side. They're tests of their own so CI runs
# them side by side, each on a machine of its own.
{ self, pkgs }:
name: script:
let
  inherit (import ./stand-ins.nix { inherit pkgs; }) claude gh;
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
  inherit name;

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
        # that binds before the slots run out (proj's pick tests have those).
        maxJobs = 3;
        pipelineSlots = 1;
        maxJobsPerProject = 3;
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
      "d /data/dev/src/uk 0755 dev dev -"
    ];
    virtualisation.memorySize = 2048;
    # The test framework panics on any OOM, a cgroup's own included; a real
    # machine kills inside the cgroup, which is what's checked here.
    boot.kernel.sysctl."vm.panic_on_oom" = 0;
  };

  testScript = ''
    import json
    import re
    import time

    def proj(args):
        return machine.succeed(f"su - dev -c 'proj {args}'").strip()

    def state(job):
        return proj(f"state {job}")

    def unit(job):
        return "proj-" + machine.succeed(f"systemd-escape {job}").strip() + ".service"

    def started(job):
        d = f"/data/dev/jobs/{job}"
        return float(machine.succeed(f"stat -c %.9Y {d}/$(cat {d}/turns).slot"))

    machine.wait_for_unit("user@1001.service")
    # burrow-events' kick port: each kick's path, a line each.
    machine.succeed("""cat >/tmp/kicks.py <<'EOF'
    import http.server
    class K(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            self.rfile.read(int(self.headers.get("Content-Length") or 0))
            open("/tmp/kicks", "a").write(self.path + "\\n")
            self.send_response(202)
            self.send_header("Content-Length", "0")
            self.end_headers()
    http.server.HTTPServer(("127.0.0.1", 8788), K).serve_forever()
    EOF""")
    machine.succeed("touch /tmp/kicks && (python3 /tmp/kicks.py >/tmp/kicks.log 2>&1 &)")
    machine.wait_for_open_port(8788)

    def kicks():
        # The watchdog's are looked at apart: proj kicks it at every change.
        return [k for k in machine.succeed("cat /tmp/kicks").split() if k != "/kick/watchdog"]

    def waits(until=None, timeout=30):
        # What proj last told the watchdog; with `until`, once that holds of
        # it (a turn's end is told after its state reads as ended).
        deadline = time.monotonic() + timeout
        while True:
            w = json.loads(machine.succeed("cat /data/dev/jobs/.waits.json 2>/dev/null || echo '{\"items\": {}}'"))["items"]
            if until is None or until(w):
                return w
            assert time.monotonic() < deadline, w
            time.sleep(0.5)

    def end(job, n=1):
        return machine.succeed(f"cat /data/dev/jobs/{job}/{n}.end").strip()

    def acts_as_workers(job):
        # The workers App's socket and git's settings, and no token of the
        # user's, as the stand-in's showenv says them.
        out = proj(f"log {job}")
        assert re.fullmatch(r"did socket=/run/gh-token/workers\.sock count=[1-9][0-9]* token=", out), out
  ''
  + script;
}
