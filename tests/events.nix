# burrow-events, for real: a machine with one handler (`probe`, whose passes
# note which repo they were for and book a wake-up) checks that the service
# starts its full pass when it starts, that a delivery signed with the hook's
# secret starts the repo's pass as its own unit (polkit lets the service
# start those and nothing else), that one signed otherwise doesn't, that a
# kick on the local port does, that a wake-up a pass booked comes on time,
# that a new secret is the one checked once it's written, that a switch
# leaves the handler's units be, that no delivery is refused while the
# service restarts for a switch, crashes or is stopped, and that a stop
# answers what it had taken first.
#
#   nix build .#checks.x86_64-linux.events-service -L
{ self, pkgs }:
let
  # A pass: says which repo it was for, and books sam/later once, a few
  # seconds on.
  probe = pkgs.writeShellScript "probe" ''
    echo "''${1:-full}" >>/var/lib/probe/passes
    # A full pass that takes its time, for as long as the test says.
    while [ -z "''${1:-}" ] && [ -e /var/lib/probe/slow ]; do sleep 1; done
    if [ "''${1:-}" = sam/r ] && ! [ -e /var/lib/probe/booked ]; then
      touch /var/lib/probe/booked
      echo "{\"sam/later\": $(( $(date +%s) + 8 ))}" >/var/lib/probe/wake.json
    fi
  '';
  # A delivery whose body is sent only once the test says, after the service
  # has taken it and asked for it. Unsigned, so its answer is a 401.
  halfSent = pkgs.writeShellScript "half-sent" ''
    exec 3<>/dev/tcp/127.0.0.1/8787
    printf 'POST /burrow/hook HTTP/1.1\r\nContent-Length: 2\r\nExpect: 100-continue\r\n\r\n' >&3
    read -r _ <&3 && read -r _ <&3 && touch /tmp/half-sent
    while ! [ -e /tmp/send-rest ]; do sleep 0.1; done
    printf '{}' >&3
    cat <&3 >/tmp/half-answer
  '';
  # With burrow-events off a handler's units are still there, and a switch
  # still leaves their running passes be.
  off = (self.inputs.nixpkgs.lib.nixosSystem {
    modules = [ self.nixosModules.events {
      nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
      system.stateVersion = "26.05";
      burrow.events = {
        enable = false;
        handlers.probe = { events = [ "push" ]; unit = "probe"; perRepo = true; };
      };
      systemd.services = pkgs.lib.genAttrs [ "probe" "probe@" ] (_: { serviceConfig.ExecStart = "true"; });
    } ];
  }).config.systemd.services;
in
assert pkgs.lib.assertMsg (!off.probe.restartIfChanged && !off."probe@".restartIfChanged)
  "a handler's units restart on a switch with burrow-events off";
pkgs.testers.runNixOSTest {
  name = "events";

  nodes.machine = { config, ... }: {
    imports = [ self.nixosModules.events ];
    burrow.events = {
      enable = true;
      host = "example.org";
      secretFile = "/etc/burrow-webhook";
      handlers.probe = {
        events = [ "push" ];
        unit = "probe";
        wake = "/var/lib/probe/wake.json";
      };
    };
    # The hourly catch-up fires on the wall clock, so a run that straddles
    # the hour got a full pass of its own among the ones the test counts.
    # The test starts the catch-up itself where it checks it.
    systemd.timers.burrow-events-catch-up.enable = false;
    # A file of its own rather than a link into the store, so the test can
    # write a new secret into it.
    environment.etc."burrow-webhook" = { text = "hook-secret\n"; mode = "0600"; };
    systemd.services = pkgs.lib.genAttrs [ "probe" "probe@" ] (name: {
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${probe}${pkgs.lib.optionalString (name == "probe@") " %I"}";
        StateDirectory = "probe";
        StateDirectoryMode = "0755";
      };
    }) // {
      # Something no handler names, to show the service can't start it.
      other.serviceConfig = {
        Type = "oneshot";
        ExecStart = "${pkgs.coreutils}/bin/true";
      };
    };
    environment.systemPackages = [ pkgs.curl pkgs.openssl ];
    system.switch.enable = true;
    # A switch that changes the service, as each bump of burrow does.
    specialisation.changed.configuration.systemd.services.burrow-events.environment.CHANGED = "1";
    # A generation whose service binds the deliveries' port itself, with no
    # socket: what a switch to this one starts from, and what its revert
    # goes back to.
    specialisation.unheld.configuration = {
      burrow.events.host = pkgs.lib.mkForce null;
      burrow.events.secretFile = pkgs.lib.mkForce null;
      systemd.services.burrow-events.serviceConfig.ExecStart = pkgs.lib.mkForce
        "${pkgs.python3}/bin/python3 -m http.server 8787 --bind 127.0.0.1 --directory /etc";
    };
  };

  testScript = ''
    import hashlib
    import hmac
    import json
    import time

    def passes():
        return machine.succeed("cat /var/lib/probe/passes 2>/dev/null || true").split()

    def deliver(body, secret, event="push"):
        sig = "sha256=" + hmac.new(secret.encode(), body.encode(), hashlib.sha256).hexdigest()
        machine.succeed(f"cat >/tmp/body <<'EOF'\n{body}\nEOF")
        machine.succeed("truncate -s -1 /tmp/body")
        return machine.succeed(f"curl -s -o /dev/null -w '%{{http_code}}' -H 'X-Hub-Signature-256: {sig}' "
                               f"-H 'X-GitHub-Event: {event}' --data-binary @/tmp/body http://127.0.0.1:8787/burrow/hook").strip()

    machine.wait_for_unit("burrow-events.service")
    base = machine.succeed("readlink -f /run/current-system").strip()

    def switch(to):
        machine.succeed(f"{base}{to}/bin/switch-to-configuration test >&2")

    def main_pid():
        return machine.succeed("systemctl show -p MainPID --value burrow-events.service").strip()

    with subtest("a switch leaves a handler's running pass be"):
        for unit in ["probe", "probe@"]:
            machine.succeed(f"grep -qx X-RestartIfChanged=false /etc/systemd/system/{unit}.service")
    machine.wait_for_open_port(8787)
    machine.wait_for_open_port(8788)

    with subtest("it catches up with a full pass when it starts"):
        machine.wait_until_succeeds("grep -x full /var/lib/probe/passes", timeout=60)

    with subtest("a signed delivery starts the repo's own pass, a forged one nothing"):
        body = json.dumps({"repository": {"full_name": "sam/r"}})
        assert deliver(body, "not-the-secret") == "401"
        assert deliver(body, "hook-secret", "issues") == "202"
        assert deliver(body, "hook-secret") == "202"
        machine.wait_until_succeeds("grep -x sam/r /var/lib/probe/passes", timeout=60)
        assert passes()[:2] == ["full", "sam/r"], passes()
        machine.succeed("systemctl show -p Result probe@sam-r.service | grep -x Result=success")

    with subtest("a wake-up the pass booked comes on time"):
        machine.wait_until_succeeds("grep -x sam/later /var/lib/probe/passes", timeout=60)

    with subtest("a kick starts a pass, a full one with no repo"):
        machine.succeed("curl -fsS -d ''' http://127.0.0.1:8788/kick/probe/sam/q")
        machine.wait_until_succeeds("grep -x sam/q /var/lib/probe/passes", timeout=60)
        machine.succeed("curl -fsS -d ''' http://127.0.0.1:8788/kick/probe")
        machine.wait_until_succeeds("[ $(grep -cx full /var/lib/probe/passes) = 2 ]", timeout=60)

    with subtest("polkit lets it start its handlers' units and nothing else"):
        machine.fail("su -s /bin/sh burrow-events -c 'systemctl start other.service'")
        machine.succeed("su -s /bin/sh burrow-events -c 'systemctl start probe@sam-x.service'")
        machine.succeed("grep -x sam/x /var/lib/probe/passes")

    # Before the new secret below: a switch writes the first one back.
    with subtest("a switch that restarts it refuses no delivery"):
        body = json.dumps({"repository": {"full_name": "sam/s"}})
        sig = "sha256=" + hmac.new(b"hook-secret", body.encode(), hashlib.sha256).hexdigest()
        machine.succeed(f"printf %s '{body}' >/tmp/sent-body")
        for to in ["/specialisation/changed", ""]:
            was = main_pid()
            machine.succeed("rm -f /tmp/codes /tmp/sender-done; touch /tmp/sending")
            machine.succeed(
                "( while [ -e /tmp/sending ]; do "
                "curl -s -m 30 -o /dev/null -w '%{http_code}\\n' "
                f"-H 'X-Hub-Signature-256: {sig}' -H 'X-GitHub-Event: issues' "
                "--data-binary @/tmp/sent-body http://127.0.0.1:8787/burrow/hook >>/tmp/codes || true; "
                "sleep 0.02; done; touch /tmp/sender-done ) >/dev/null 2>&1 &")
            machine.wait_until_succeeds("test -s /tmp/codes", timeout=30)
            switch(to)
            assert main_pid() != was, "the switch didn't restart burrow-events"
            time.sleep(2)
            machine.succeed("rm /tmp/sending")
            machine.wait_until_succeeds("test -e /tmp/sender-done", timeout=60)
            codes = machine.succeed("cat /tmp/codes").split()
            assert set(codes) == {"202"}, f"a switch lost deliveries: { {c: codes.count(c) for c in set(codes)} }"

    with subtest("one that comes while it's crashed or stopped waits for it"):
        body = json.dumps({"repository": {"full_name": "sam/d"}})
        for down in ["systemctl kill -s KILL", "systemctl stop"]:
            was = main_pid()
            machine.succeed(f"{down} burrow-events.service")
            machine.wait_until_succeeds(f"[ $(systemctl show -p MainPID --value burrow-events.service) != {was} ]", timeout=30)
            assert deliver(body, "hook-secret", "issues") == "202", down

    with subtest("a switch goes back to a service that binds the port itself, and on again"):
        switch("/specialisation/unheld")
        machine.wait_until_succeeds("curl -fsS -o /dev/null http://127.0.0.1:8787/os-release", timeout=30)
        switch("")
        machine.wait_for_unit("burrow-events.service")
        assert deliver(body, "hook-secret", "issues") == "202"

    with subtest("a new secret restarts it once a running full pass is done, and is the one checked"):
        # A full pass that read the old secret is still going.
        machine.succeed("touch /var/lib/probe/slow")
        machine.succeed("curl -fsS -d ''' http://127.0.0.1:8788/kick/probe")
        machine.wait_until_succeeds("systemctl show -p ActiveState --value probe.service | grep -x activating", timeout=60)
        fulls = passes().count("full")
        pid = machine.succeed("systemctl show -p MainPID --value burrow-events.service").strip()
        machine.succeed("echo new-secret >/etc/burrow-webhook")
        time.sleep(5)
        assert machine.succeed("systemctl show -p MainPID --value burrow-events.service").strip() == pid
        machine.succeed("rm /var/lib/probe/slow")
        machine.wait_until_succeeds(f"[ $(systemctl show -p MainPID --value burrow-events.service) != {pid} ]", timeout=60)
        # Its catch-up is a full pass of its own, after the old one.
        machine.wait_until_succeeds(f"[ $(grep -cx full /var/lib/probe/passes) -gt {fulls} ]", timeout=60)
        machine.wait_for_open_port(8787)
        body = json.dumps({"repository": {"full_name": "sam/n"}})
        assert deliver(body, "hook-secret") == "401"
        assert deliver(body, "new-secret") == "202"
        machine.wait_until_succeeds("grep -x sam/n /var/lib/probe/passes", timeout=60)

    with subtest("a stop answers what it took first, and leaves a running pass's start be"):
        # systemctl waits on this one while the service drains.
        machine.succeed("touch /var/lib/probe/slow")
        machine.succeed("curl -fsS -d ''' http://127.0.0.1:8788/kick/probe")
        machine.wait_until_succeeds("systemctl show -p ActiveState --value probe.service | grep -x activating", timeout=60)
        machine.succeed("${halfSent} >/dev/null 2>&1 &")
        machine.wait_until_succeeds("test -e /tmp/half-sent", timeout=30)
        failed = machine.succeed("journalctl -u burrow-events.service | grep -c 'failed; again' || true")
        machine.succeed("systemctl stop --no-block burrow-events.service")
        time.sleep(1)
        machine.succeed("touch /tmp/send-rest")
        machine.wait_until_succeeds("systemctl show -p ActiveState --value burrow-events.service | grep -x inactive", timeout=30)
        machine.wait_until_succeeds("grep -q '^HTTP/1.1 401 ' /tmp/half-answer", timeout=10)
        machine.succeed("journalctl --sync")
        assert machine.succeed("journalctl -u burrow-events.service | grep -c 'failed; again' || true") == failed, "a stop failed a running pass"
        machine.succeed("rm /var/lib/probe/slow")
        machine.succeed("systemctl start burrow-events.service")
        machine.wait_for_open_port(8788)

    with subtest("the catch-up doesn't fail while burrow-events is down"):
        # Its own start runs every full pass anyway; a failed unit would read
        # as a broken switch.
        machine.succeed("systemctl stop burrow-events.service")
        machine.succeed("systemctl start burrow-events-catch-up.service")
        machine.succeed("journalctl -u burrow-events-catch-up.service | grep \"couldn't kick probe\"")
        machine.succeed("systemctl start burrow-events.service")
  '';
}
