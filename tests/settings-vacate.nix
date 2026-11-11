# A real user manager, both engine pools, an inherited heavy lock and native
# proof fences. Only engine/GitHub replies are fixtures.
{ self, pkgs }:
let
  inherit (import ./proj/stand-ins.nix { inherit pkgs; }) gh;
  engine = name: pkgs.writeShellScriptBin name ''
    prompt=$(cat)
    case $prompt in *$'\n## This turn, asked\n'*) prompt=''${prompt##*$'\n## This turn, asked\n\n'} ;; esac
    id="session-$PROJ_JOB"
    if [ ${name} = codex ]; then
      printf '{"type":"thread.started","thread_id":"%s"}\n' "$id"
    else
      printf '{"type":"system","session_id":"%s"}\n' "$id"
    fi
    finish() {
      if [ ${name} = codex ]; then
        printf '{"type":"item.completed","item":{"type":"agent_message","text":"continued"}}\n'
        printf '{"type":"turn.completed","usage":{"input_tokens":0,"output_tokens":0}}\n'
      else
        printf '{"type":"result","session_id":"%s","result":"continued","is_error":false}\n' "$id"
      fi
    }
    case " $* " in *" --resume "*|*" resume "*) echo resumed >"$PWD/resumed-$PROJ_JOB"; finish; exit 0 ;; esac
    case "$prompt" in
      *fixture-finish*) proj done >/dev/null; finish; exit 0 ;;
      *fixture-ask*) proj ask --default keep >/dev/null; finish; exit 0 ;;
      *fixture-await*) proj await https://github.com/o/r/pull/1 >/dev/null; finish; exit 0 ;;
    esac
    mkdir -p "$PROJ_SCRATCH"
    echo preserved >"$PROJ_SCRATCH/preserved"
    git branch "kept-$PROJ_JOB"
    while [ ! -e /data/finish-held ]; do sleep 0.2; done
    finish
  '';
in pkgs.testers.runNixOSTest {
  name = "settings-vacate";
  nodes.machine = { config, lib, ... }: {
    imports = [ self.nixosModules.proj self.nixosModules.cpu ((import ../examples/machine.nix { inherit lib; }) // { nixpkgs={}; boot={}; fileSystems={}; }) ];
    burrow = {
      claude.package = engine "claude";
      cpu.enable = true;
      settings.allowed = [ "o/r" "o/web" ];
      github = {
        apps.workers = { id=7; slug="test-workers"; keyFile="/run/keys/workers.pem"; users=[ "dev" ]; };
        machinery = "workers";
      };
      proj = {
        enable=true;
        inherit gh;
        maxJobs=1; pipelineSlots=0; maxJobsPerProject=10; minFreeMemory=0;
        codex = { slots=1; package=engine "codex"; };
        inbox = { enable=true; planning=false; authors=[ "sam" ]; triage = { enable=true; drain="codex"; drainJobs=1; }; };
      };
    };
    environment.systemPackages = [ pkgs.git pkgs.jq self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-proof ];
    systemd.tmpfiles.rules = [
      "d /data/dev/src/ib 0755 dev dev -"
      "d /data/dev/src/web 0755 dev dev -"
      "d /data/proof-checkout 0755 dev dev -"
      "d /data/proof-trees 0755 dev dev -"
      "d /var/lib/burrow/autoland/builds 0755 dev dev -"
      "d /home/dev/.codex 0700 dev dev -"
    ];
    environment.etc."test-proof-tools".text = builtins.toJSON {
      bash="${pkgs.bash}/bin/bash"; git="${config.programs.git.package}/bin/git"; heavy="${config.burrow.tools.heavy}/bin/heavy";
    };
  };
  testScript = ''
    import json, shlex
    machine.wait_for_unit("user@1001.service")
    machine.succeed("systemctl mask --runtime --now burrow-events.service proj-inbox.service proj-inbox@.service proj-watch.service proj-watch@.service")
    def dev(cmd):
        return machine.succeed("runuser -u dev -- bash -lc " + shlex.quote(cmd)).strip()
    def state(j):
        return dev("proj state " + j)
    def eventually(j, expected):
        machine.wait_until_succeeds("runuser -u dev -- proj state " + j + " | grep -x " + expected, timeout=45)
    def data(endpoint, value):
        path="/tmp/gh/"+endpoint.replace("/", "_")
        machine.succeed("mkdir -p /tmp/gh && printf %s " + shlex.quote(json.dumps(value)) + " >" + path + "; chmod -R a+rwX /tmp/gh")
    dev("git -C /data/dev/src/ib init -q && git -C /data/dev/src/ib -c user.name=Fixture -c user.email=fixture@example.org commit -q --allow-empty -m initial")
    dev("git -C /data/dev/src/web init -q && git -C /data/dev/src/web -c user.name=Fixture -c user.email=fixture@example.org commit -q --allow-empty -m initial")
    machine.succeed("install -d -o dev -g dev /srv")
    dev("git clone -q --bare /data/dev/src/ib /srv/origin.git && git -C /data/dev/src/ib push -q /srv/origin.git HEAD:refs/pull/1/head && git clone -q /srv/origin.git /data/proof-checkout")
    dev("git -C /data/dev/src/ib remote add origin https://github.com/o/r && git -C /data/dev/src/web remote add origin https://github.com/o/web")
    for path,val in {
      "repos/o/r/issues":[], "repos/o/r/issues/1":{"pull_request":{},"state":"open","comments":0},
      "repos/o/r/pulls/1":{"state":"open","merged":False,"head":{"sha":"a"},"comments":0,"review_comments":0},
      "repos/o/r/pulls/1/reviews":[], "repos/o/r/commits/a/check-runs":{"check_runs":[]},
      "repos/o/r/commits/a/status":{"state":"success","total_count":0}, "repos/o/r/issues/1/comments":[]
    }.items(): data(path,val)
    opts="--owned --kick proj-inbox/o/r "
    ask=dev("proj run "+opts+"ib fixture-ask")
    eventually(ask,"done")
    aw=dev("proj run "+opts+"ib fixture-await")
    eventually(aw,"done")
    question=machine.succeed("cat /var/lib/burrow/proj-questions/"+ask+"/question")
    awaiting=machine.succeed("cat /data/dev/jobs/"+aw+"/await").split("\t")
    run=dev("proj run "+opts+"ib hold")
    eventually(run,"running")
    queued=dev("proj run --first "+opts+"ib hold")
    eventually(queued,"queued")
    queued_turn=machine.succeed("cat /data/dev/jobs/"+queued+"/turns")
    dev("mkdir -p ~/.config/proj && printf 'coding codex\\n' >~/.config/proj/roles")
    code=dev("proj run --role coding "+opts+"ib hold")
    eventually(code,"running")
    for j in [run,code]: machine.wait_until_succeeds("test -s /data/dev/jobs/"+j+"/session")
    sessions={j:machine.succeed("cat /data/dev/jobs/"+j+"/session") for j in [run,code]}
    machine.succeed("install -d -o dev -g dev /var/lib/burrow/proj-inbox/o_r-20")
    dev("printf 'o/r#20\\n' >/var/lib/burrow/proj-inbox/o_r-20/drained; printf '%s\\n' "+code+" >/var/lib/burrow/proj-inbox/o_r-20/job")
    head=dev("git -C /data/dev/src/ib rev-parse HEAD")
    tools=json.loads(machine.succeed("cat /etc/test-proof-tools"))
    proof="proof-o-r-1-aaaaaaaaaaaa-1"
    folder="/var/lib/burrow/autoland/builds/"+proof
    record={"repo":"o/r","number":1,"head":head,"checkout":"/data/proof-checkout","unit":"autoland-"+proof+".service","timeout":600,"bash":tools["bash"],"git":tools["git"],"trees":"/data/proof-trees","kick":None,"commentBytes":1000,
      "runs":{"a-complete":"echo complete >>/data/proof-events", "b-hold":"echo preserved >preserved; if [ ! -e /data/release-proof ]; then "+tools["heavy"]+" --all -- sleep 600; fi; echo resumed >>/data/proof-events"}}
    machine.succeed("install -d -o dev -g dev "+folder+"; touch /data/proof-events; chown dev:dev /data/proof-events")
    machine.succeed("printf %s "+shlex.quote(json.dumps(record))+" >"+folder+"/build.json; chown dev:dev "+folder+"/build.json")
    dev("burrow-proof start "+folder)
    machine.wait_until_succeeds("test -s "+folder+"/ran.json && test -e /data/proof-trees/"+proof+"-1/preserved")
    machine.wait_until_fails("runuser -u dev -- heavy --wait 1 -- true", timeout=15)
    permitted=dev("proj run --owned --kick proj-inbox/o/web web fixture-finish")
    eventually(permitted,"queued")
    dev("settings pause o/r")
    for j in [ask,aw,run,queued,code]: eventually(j,"parked")
    eventually(permitted,"done")
    for j in [run,queued,code]:
        machine.fail("runuser -u dev -- systemctl --user is-active 'proj-"+j.replace("-","\\x2d")+".service'")
    machine.wait_until_succeeds("test -e "+folder+"/policy-paused")
    machine.wait_until_succeeds("runuser -u dev -- heavy --wait 1 -- true")
    for j in [run,code]:
        machine.succeed("test -f /data/dev/scratch/jobs/"+j+"/preserved")
        assert machine.succeed("cat /data/dev/jobs/"+j+"/session")==sessions[j]
    assert machine.succeed("cat /var/lib/burrow/proj-questions/"+ask+"/question")==question
    assert machine.succeed("cat /data/dev/jobs/"+aw+"/await").split("\t")==awaiting
    machine.succeed("test -e /data/proof-trees/"+proof+"-1/preserved; test $(grep -c complete /data/proof-events) = 1")
    # A paused drained record no longer reserves the only drain handoff.
    issue={"number":30,"user":{"login":"sam","type":"User"},"html_url":"https://github.com/o/web/issues/30","repository_url":"https://api.github.com/repos/o/web","title":"fixture","body":"fixture-finish","state":"open","labels":[{"name":"ready"}],"created_at":"2026-01-01T00:00:00Z"}
    data("repos/o/web/issues",[issue]); data("repos/o/web/issues/30",issue); data("repos/o/web/issues/30/comments",[])
    machine.succeed("touch /tmp/gh/calls; chmod a+rw /tmp/gh/calls")
    dev("proj-inbox o/web")
    machine.succeed("grep -F 'POST repos/o/web/issues/30/labels' /tmp/gh/calls")
    issue["labels"].append({"name":"codex"}); data("repos/o/web/issues",[issue]); data("repos/o/web/issues/30",issue)
    dev("proj-inbox o/web")
    fresh=machine.succeed("cat /var/lib/burrow/proj-inbox/o_web-30/job").strip()
    eventually(fresh,"done")
    # Restore with the completed proof check, conversation and order intact.
    machine.succeed("touch /data/release-proof")
    dev("settings allow o/r")
    eventually(queued,"running")
    eventually(run,"queued")
    assert machine.succeed("cat /data/dev/jobs/"+queued+"/turns")==queued_turn
    machine.succeed("touch /data/finish-held")
    eventually(queued,"done")
    for j in [run,code]:
        eventually(j,"done")
        machine.succeed("test -f /data/dev/src/ib/resumed-"+j)
        assert machine.succeed("cat /data/dev/jobs/"+j+"/session")==sessions[j]
    assert dev("proj state "+ask)=="done" and "[asks]" in dev("proj status 100")
    assert machine.succeed("cat /data/dev/jobs/"+aw+"/await").split("\t")[0]==awaiting[0]
    machine.wait_until_succeeds("jq -e '.state == \"done\"' "+folder+"/end.json")
    machine.succeed("test $(grep -c complete /data/proof-events) = 1; grep -x resumed /data/proof-events")
  '';
}
