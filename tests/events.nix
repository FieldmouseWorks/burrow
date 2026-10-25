# burrow-events, for real: a machine with one handler (`probe`, whose passes
# note which repo they were for and book a wake-up) checks that the service
# starts its full pass when it starts, that a delivery signed with the hook's
# secret starts the repo's pass as its own unit (polkit lets the service
# start those and nothing else), that one signed otherwise doesn't, that a
# kick on the local port does, that a wake-up a pass booked comes on time,
# and that a new secret is the one checked once it's written.
#
#   nix build .#checks.x86_64-linux.events-service -L
{ self, pkgs }:
let
  # A pass: says which repo it was for, and books sam/later once, a few
  # seconds on.
  probe = pkgs.writeShellScript "probe" ''
    echo "''${1:-full}" >>/var/lib/probe/passes
    if [ "''${1:-}" = sam/r ] && ! [ -e /var/lib/probe/booked ]; then
      touch /var/lib/probe/booked
      echo "{\"sam/later\": $(( $(date +%s) + 8 ))}" >/var/lib/probe/wake.json
    fi
  '';
in
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
  };

  testScript = ''
    import hashlib
    import hmac
    import json

    def passes():
        return machine.succeed("cat /var/lib/probe/passes 2>/dev/null || true").split()

    def deliver(body, secret, event="push"):
        sig = "sha256=" + hmac.new(secret.encode(), body.encode(), hashlib.sha256).hexdigest()
        machine.succeed(f"cat >/tmp/body <<'EOF'\n{body}\nEOF")
        machine.succeed("truncate -s -1 /tmp/body")
        return machine.succeed(f"curl -s -o /dev/null -w '%{{http_code}}' -H 'X-Hub-Signature-256: {sig}' "
                               f"-H 'X-GitHub-Event: {event}' --data-binary @/tmp/body http://127.0.0.1:8787/burrow/hook").strip()

    machine.wait_for_unit("burrow-events.service")
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

    with subtest("a new secret restarts it, and is the one checked"):
        pid = machine.succeed("systemctl show -p MainPID --value burrow-events.service").strip()
        machine.succeed("echo new-secret >/etc/burrow-webhook")
        machine.wait_until_succeeds(f"[ $(systemctl show -p MainPID --value burrow-events.service) != {pid} ]", timeout=60)
        machine.wait_for_open_port(8787)
        body = json.dumps({"repository": {"full_name": "sam/n"}})
        assert deliver(body, "hook-secret") == "401"
        assert deliver(body, "new-secret") == "202"
        machine.wait_until_succeeds("grep -x sam/n /var/lib/probe/passes", timeout=60)
  '';
}
