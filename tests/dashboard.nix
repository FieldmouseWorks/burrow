# The dashboard as its module runs it: the service is reached on localhost
# and nowhere else (with the firewall off, so it's the service that says
# so, not the firewall), a page asked for with no session, or a forged one,
# is sent to sign in, a POST from another origin is turned away, and
# enrolment refuses a wrong code and takes the right one, which only root
# can print. The ceremonies themselves, with a software authenticator, and
# when sessions end, are crates/burrow-dashboard's tests; `tailscale serve`
# needs a tailnet, so it's evaluated here (the standalone check) but not run.
#
#   nix build .#checks.x86_64-linux.dashboard -L
{ self, pkgs }:
let
  host = "machine.tail1234.ts.net";
  origin = "https://${host}";
  port = 8473;
  local = "http://127.0.0.1:${toString port}";
in
pkgs.testers.runNixOSTest {
  name = "dashboard";

  nodes = {
    machine = {
      imports = [ self.nixosModules.dashboard ];
      burrow.dashboard = {
        enable = true;
        inherit host port;
        owner = { id = 583231; login = "sam"; };
        serve.enable = false;
      };
      networking.firewall.enable = false;
      virtualisation.vlans = [ 1 ];
      users.users.dev = {
        isNormalUser = true;
        uid = 1001;
        group = "dev";
      };
      users.groups.dev.gid = 1001;
      environment.systemPackages = [ pkgs.curl pkgs.jq pkgs.iproute2 ];
    };
    client = {
      virtualisation.vlans = [ 1 ];
      environment.systemPackages = [ pkgs.curl ];
    };
  };

  testScript = ''
    import json, shlex

    def post(path, body, origin=None):
        """POST JSON as a page would: the status, and the answer in /tmp/answer."""
        o = f"-H {shlex.quote('Origin: ' + origin)}" if origin else ""
        return machine.succeed(
            "curl -sS -o /tmp/answer -w '%{http_code}' -H 'Content-Type: application/json' "
            f"{o} -d {shlex.quote(json.dumps(body))} ${local}{path}"
        )

    start_all()
    machine.wait_for_unit("burrow-dashboard.service")
    machine.wait_until_succeeds("curl -fsS -o /dev/null ${local}/login")

    with subtest("it listens on localhost and nowhere else"):
        listening = machine.succeed("ss -Hltn 'sport = :${toString port}' | awk '{print $4}'").split()
        assert listening == ["127.0.0.1:${toString port}"], listening
        addr = machine.succeed("ip -4 -o addr show dev eth1 | awk '{print $4}' | cut -d/ -f1").strip()
        machine.fail(f"curl -sS --max-time 5 http://{addr}:${toString port}/login")
        client.wait_for_unit("multi-user.target")
        client.succeed("ping -c1 machine")
        client.fail("curl -sS --max-time 5 http://machine:${toString port}/login")

    with subtest("a page with no session is sent to sign in"):
        for cookie in ["", "-H 'Cookie: __Host-session=forged'"]:
            got = machine.succeed(f"curl -sS -o /dev/null -w '%{{http_code}} %{{redirect_url}}' {cookie} ${local}/")
            assert got == "303 ${local}/login", got
        page = machine.succeed("curl -fsS ${local}/login")
        assert "Add this device" in page, page

    with subtest("only root prints a code"):
        machine.fail("su dev -c 'burrow-dashboard enroll'")
        code = machine.succeed("burrow-dashboard enroll 2>/dev/null").strip()
        print(f"code: {code}")
        assert machine.succeed("stat -c '%U %a' /var/lib/burrow/dashboard/enroll.json").strip() == "burrow-dashboard 600"
        assert machine.succeed("stat -c '%U %a' /var/lib/burrow/dashboard").strip() == "burrow-dashboard 700"

    with subtest("a POST from another origin is turned away"):
        for origin in [None, "https://elsewhere.example"]:
            got = post("/enroll/start", {"code": code}, origin)
            assert got == "403", (origin, got)

    with subtest("enrolment refuses a wrong code"):
        got = post("/enroll/start", {"code": "WRONG-CODE0"}, "${origin}")
        answer = machine.succeed("cat /tmp/answer")
        assert got == "403", (got, answer)
        assert "4 more tries" in answer, answer
        machine.fail("jq -e .options /tmp/answer")

    with subtest("and takes the right one"):
        got = post("/enroll/start", {"code": code, "name": "Phone"}, "${origin}")
        assert got == "200", (got, machine.succeed("cat /tmp/answer"))
        machine.succeed("jq -e '.options.publicKey.rp.id == \"${host}\"' /tmp/answer")
        machine.succeed("jq -e '.options.publicKey.user.name == \"sam\"' /tmp/answer")
        machine.succeed("jq -e '.options.publicKey.authenticatorSelection.userVerification == \"required\"' /tmp/answer")
  '';
}
