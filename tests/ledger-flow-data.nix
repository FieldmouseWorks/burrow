# The flow's state (Conary's docs/flow.md, "State and workers"), carried
# over whenever who runs the flow changes (burrow.ledger.flow.data,
# crates/burrow-ledger-access's `flow`). The flow, as burrow.user, keeps a
# PR's record and a pause; the controller turned on (burrow.ledger
# .controllerUser) still sees both, and so does the next one when it's
# changed, and burrow.user again when it's turned off, the user it had
# keeping none of it. A worker still running as the user it had holds the
# carry off until it exits, two histories are refused, not merged, and a
# link the old user leaves in the state leads it nowhere; each time, the
# flow doesn't start on no state.
#
# cn here is a stand-in that keeps `flow pause` and reads `flow status`
# where Conary does (conary-github's flow/persist.rs and pause.rs: the
# repo's directory under CONARY_DATA's flow/, its `open`, its records and
# its `pause`). `conary` runs it on a real Conary's cn instead:
#
#   nix build .#checks.x86_64-linux.ledger-flow-data -L
#   nix build --impure --expr '(builtins.getFlake "path:'$PWD'").inputs.nixpkgs.legacyPackages.x86_64-linux.callPackage ./tests/ledger-flow-data.nix { self = builtins.getFlake "path:'$PWD'"; conary = CONARY; }'
{ self, pkgs, conary ? null }:
let
  standIn = pkgs.writeShellApplication {
    name = "cn";
    runtimeInputs = [ pkgs.coreutils pkgs.jq ];
    text = ''
      root=''${CONARY_DATA:-''${XDG_STATE_HOME:-$HOME/.local/state}/conary/data}
      case "$1 ''${2:-}" in
        "flow pause")
          d=$root/flow/''${3,,}
          mkdir -p "$d"
          jq -cn --arg reason "$5" '{scope: "repository", reason: $reason, at: 1}' >"$d/pause"
          ;;
        "flow status")
          d=$root/flow/''${3,,}
          read_at=null switches='[]' degraded='[]'
          [ -e "$d/open" ] && read_at=$(jq -e .at "$d/open")
          [ -e "$d/pause" ] && switches=$(jq -c '[{switch: "pause", reason}]' "$d/pause")
          for pr in $( [ -e "$d/open" ] && jq -r '.prs[]' "$d/open"); do
            if [ -e "$d/$pr.json" ] && ! jq -e .memory "$d/$pr.json" >/dev/null; then
              degraded=$(jq -c --argjson pr "$pr" '. + [{pr: $pr, path: "record"}]' <<<"$degraded")
            fi
          done
          jq -n --argjson r "$read_at" --argjson s "$switches" --argjson d "$degraded" \
            '{read_at: $r, switches: $s} + (if $d == [] then {} else {degraded: $d} end)'
          ;;
        # The ledger's build loads its policy.
        *) exit 0 ;;
      esac
    '';
  };
  cn = if conary == null then standIn else conary;
  # A PR's record as Conary saves one (conary-github's persist.rs Record),
  # its memory holding a review asked and a stall clock, and the PRs the
  # flow last read open.
  record = pkgs.writeText "5.json" (builtins.toJSON {
    memory = {
      head = "aaaa";
      raised = [ ];
      judge_issuance = [ ];
      major_issuance = [ ];
      seen = 0;
      no_checks_since = null;
      update_requested = null;
      strikes = [ ];
      retry = null;
      asked = [ "aaaa" 100 ];
      handed = [ ];
      lost_jobs = [ ];
      unanswered = [ ];
      broken_jobs = [ ];
      answered_jobs = [ ];
      stopping = [ ];
      retired_jobs = [ ];
      unposted = [ ];
      push_looks = [ ];
      wait = { head = "aaaa"; base = "main"; stage = "review"; description = "Waiting on review"; failure = null; since = 100; due = 200; };
    };
    decision = null;
    jobs = [ ];
    failed = null;
    succeeded = [ ];
    sequence = 3;
  });
  open = pkgs.writeText "open" (builtins.toJSON { at = 1; prs = [ 5 ]; });
  flowConfig = pkgs.writeText "flow.json" "{}";
in
pkgs.testers.runNixOSTest {
  name = "ledger-flow-data";

  nodes.machine = { config, lib, ... }: let ledger = config.burrow.ledger; in {
    imports = [ self.nixosModules.ledger ];
    burrow.ledger = {
      enable = true;
      package = cn;
      owners = [ "sam" ];
      anchorRepo = "sam/homelab";
      reviewers = [ "steward[bot]" ];
    };
    burrow.events.enable = lib.mkForce false;
    users.users.dev.isNormalUser = true;
    environment.systemPackages = [ cn pkgs.util-linux ];
    # The controller turned on, and changed; the machine itself has none.
    specialisation = {
      on.configuration.burrow.ledger.controllerUser = "conary-flow";
      moved.configuration.burrow.ledger.controllerUser = "conary-flow2";
    };
    # `cn flow status` as the flow runs: as its user, on its state, as
    # conaryd is given it (modules/autoland/landing.nix).
    systemd.services.flow-status = {
      requires = ledger.flow.needs or [ ];
      after = ledger.flow.needs or [ ];
      environment = ledger.env // ledger.flow.env // {
        CONARY_DATA = ledger.flow.env.CONARY_DATA or "${ledger.dir}/data";
      };
      serviceConfig = {
        Type = "oneshot";
        User = ledger.flow.user;
        ExecStart = "${cn}/bin/cn flow status sam/plain --config ${flowConfig}";
        StandardOutput = "truncate:/run/flow-status.json";
      };
    };
  };

  testScript = ''
    import json

    ledger = "/var/lib/burrow/conary"
    old = f"{ledger}/data"
    flow = "/var/lib/burrow/flow"
    repo = "flow/sam/plain"
    record = "${record}"
    base = "/run/booted-system/bin/switch-to-configuration test"

    def to(name):
        return f"/run/booted-system/specialisation/{name}/bin/switch-to-configuration test"

    def status():
        machine.succeed("systemctl start flow-status.service")
        return json.loads(machine.succeed("cat /run/flow-status.json"))

    def shows_both(where, user, carried=True):
        s = status()
        assert s["read_at"] == 1, s
        assert [w.get("reason") for w in s["switches"] if w["switch"] == "pause"] == ["held"], s
        assert not s.get("degraded"), s
        assert machine.succeed(f"runuser -u {user} -- cat {where}/{repo}/5.json") == machine.succeed(f"cat {record}")
        if not carried:
            return
        # All of it the user's, in a directory only it may enter.
        assert machine.succeed(f"find {where} -not -user {user} -o -not -group {user if user != 'dev' else 'users'}") == ""
        assert machine.succeed(f"stat -c '%U %a' {where}").strip() == f"{user} 700"

    def unreadable(where, uid):
        # By number: a switch removes the user it had.
        as_it = f"setpriv --reuid={uid} --regid={uid} --clear-groups"
        machine.fail(f"{as_it} ls {where}")
        machine.fail(f"{as_it} cat {where}/{repo}/5.json")
        assert machine.succeed(f"find {where} -uid {uid}") == ""

    start_all()
    machine.wait_for_unit("multi-user.target")

    with subtest("as burrow.user, the flow keeps a PR's record and a pause"):
        machine.succeed(f"runuser -u dev -- env CONARY_DATA={old} cn flow pause sam/plain --reason held")
        machine.succeed(f"runuser -u dev -- cp ${open} {old}/{repo}/open")
        machine.succeed(f"runuser -u dev -- cp {record} {old}/{repo}/5.json")
        shows_both(old, "dev", carried=False)

    with subtest("the controller turned on still shows both"):
        machine.succeed(to("on"))
        shows_both(flow, "conary-flow")
        machine.fail(f"test -e {old}")
        unreadable(flow, machine.succeed("id -u dev").strip())

    with subtest("changed, the next controller has it, and the last keeps none"):
        first = machine.succeed("id -u conary-flow").strip()
        machine.succeed(to("moved"))
        shows_both(flow, "conary-flow2")
        unreadable(flow, first)

    with subtest("turned off, burrow.user's flow has it back"):
        second = machine.succeed("id -u conary-flow2").strip()
        machine.succeed(base)
        shows_both(old, "dev")
        machine.fail(f"test -e {flow}")
        unreadable(old, second)

    with subtest("a worker still running as the user it had holds the carry off"):
        machine.succeed(f"runuser -u dev -- touch {old}/{repo}/5-1.running")
        machine.succeed(f"systemd-run --unit=worker -p User=dev flock {old}/{repo}/5-1.running ${pkgs.coreutils}/bin/sleep infinity")
        machine.wait_until_succeeds(f"! flock -n {old}/{repo}/5-1.running true")
        machine.fail(to("on"))
        machine.succeed("systemctl is-failed burrow-flow-data.service")
        # The flow doesn't start on no state.
        machine.fail("systemctl start flow-status.service")
        machine.fail(f"test -e {flow}")
        machine.succeed(f"test -e {old}/{repo}/5.json")
        machine.succeed("systemctl stop worker.service")
        # Its next start carries it.
        shows_both(flow, "conary-flow")

    with subtest("two histories are refused, not merged"):
        machine.succeed(f"runuser -u dev -- sh -c 'mkdir -p {old}/{repo} && echo {{}} >{old}/{repo}/7.json'")
        machine.fail(to("moved"))
        machine.succeed("systemctl is-failed burrow-flow-data.service")
        machine.fail("systemctl start flow-status.service")
        machine.succeed(f"test -e {old}/{repo}/7.json")
        machine.fail(f"test -e {old}/{repo}/5.json")
        machine.succeed(f"test -e {flow}/{repo}/5.json")
        # The one the flow shouldn't carry on from moved aside.
        machine.succeed(f"rm -r {old}")
        shows_both(flow, "conary-flow2")

    with subtest("a link the user it had leaves leads it nowhere"):
        machine.succeed("echo root >/var/lib/root-secret && chmod 600 /var/lib/root-secret")
        machine.succeed(f"runuser -u conary-flow2 -- ln -s /var/lib/root-secret {flow}/{repo}/6.json")
        machine.fail(base)
        machine.succeed("systemctl is-failed burrow-flow-data.service")
        machine.fail("systemctl start flow-status.service")
        assert machine.succeed("stat -c '%U %a' /var/lib/root-secret").strip() == "root 600"
        machine.fail("runuser -u dev -- cat /var/lib/root-secret")
        machine.fail(f"test -e {old}")
        machine.succeed(f"rm {flow}/{repo}/6.json")
        shows_both(old, "dev")
  '';
}
