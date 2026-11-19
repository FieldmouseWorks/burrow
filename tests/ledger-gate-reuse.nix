# The ledger's gate-reuse lookup (burrow.ledger.gateReuse), running: on, a
# conaryd of its own answers an unsigned request with a refusal, over
# loopback and over the vhost's HTTPS path, with a configuration whose
# signer is the train's and whose token is the ledger's own; the socket's
# conaryd keeps no network. Off, nothing listens. Which config it's given
# is checks/ledger-gate-reuse-config.nix's.
#
# burrow has no Conary of its own, so conaryd here is a stand-in that reads
# its configuration as Conary's reader does (`Settings`, in conary-github's
# reuse.rs: these fields and no others) and refuses an unsigned request.
# `conary` runs it on a real Conary instead:
#
#   nix build .#checks.x86_64-linux.ledger-gate-reuse -L
#   nix build --impure --expr '(builtins.getFlake "path:'$PWD'").inputs.nixpkgs.legacyPackages.x86_64-linux.callPackage ./tests/ledger-gate-reuse.nix { self = builtins.getFlake "path:'$PWD'"; conary = CONARY; }'
{ self, pkgs, conary ? null }:
let
  # GitHub's answer to the token the user's gh login has.
  gh = pkgs.writeShellScriptBin "gh" ''
    case "$*" in
      "auth token") echo gho_stand_in ;;
      *) echo "gh $*: not a stand-in answer" >&2; exit 1 ;;
    esac
  '';
  conaryd = pkgs.writers.writePython3Bin "conaryd" { } ''
    import http.server
    import ipaddress
    import json
    import os
    import socket
    import threading

    # conary-github's reuse::Settings: these fields, no others, and the
    # first four required.
    FIELDS = {"listen", "repos", "token_command", "signer", "fresh", "api",
              "server"}
    REQUIRED = {"listen", "repos", "token_command", "signer"}

    sock = socket.socket(socket.AF_UNIX)
    sock.bind(os.environ["CONARY_SOCKET"])
    sock.listen()

    path = os.environ.get("CONARY_GATE_REUSE_CONFIG")
    if path:
        with open(path) as f:
            config = json.load(f)
        assert set(config) <= FIELDS, f"unknown fields {set(config) - FIELDS}"
        assert REQUIRED <= set(config), f"missing {REQUIRED - set(config)}"
        assert all(isinstance(r, str) for r in config["repos"])
        assert isinstance(config["token_command"], str)
        assert isinstance(config["signer"], str)
        assert config.get("fresh") is None or isinstance(config["fresh"], str)
        host, port = config["listen"].rsplit(":", 1)
        host = str(ipaddress.ip_address(host.strip("[]")))

        class Lookup(http.server.BaseHTTPRequestHandler):
            def answer(self, status):
                self.send_response(status)
                self.end_headers()

            def do_GET(self):
                self.answer(401)

            def do_POST(self):
                length = int(self.headers.get("Content-Length", 0))
                try:
                    request = json.loads(self.rfile.read(length))
                except ValueError:
                    return self.answer(400)
                keys = {"contract", "repository", "commit", "tree", "gate"}
                if not isinstance(request, dict) or set(request) != keys:
                    return self.answer(400)
                if request["repository"] not in config["repos"]:
                    return self.answer(403)
                auth = self.headers.get("Authorization", "")
                if not auth.startswith("Bearer "):
                    return self.answer(401)
                # Its token can't be checked here: GitHub's keys are away.
                self.answer(503)

        class Server(http.server.ThreadingHTTPServer):
            address_family = (socket.AF_INET6 if ":" in host
                              else socket.AF_INET)

        server = Server((host, int(port)), Lookup)
        threading.Thread(target=server.serve_forever, daemon=True).start()

    while True:
        sock.accept()[0].close()
  '';
  cn = pkgs.writeShellScriptBin "cn" ''[ "$1" = policy ] && exit 0; exit 1'';
  package = if conary != null then conary else pkgs.symlinkJoin { name = "conary-stand-in"; paths = [ cn conaryd ]; };

  # The vhost's certificate, for example.org.
  tls = pkgs.runCommand "example.org-tls" { nativeBuildInputs = [ pkgs.openssl ]; } ''
    mkdir $out
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 3650 \
      -subj /CN=example.org -addext subjectAltName=DNS:example.org \
      -keyout $out/key.pem -out $out/cert.pem
  '';

  machine = gateReuse: { lib, ... }: {
    imports = [ self.nixosModules.ledger ];
    burrow.ledger = {
      enable = true;
      daemon = true;
      inherit package gh;
      owners = [ "sam" ];
      anchorRepo = "sam/homelab";
      reviewers = [ "steward[bot]" ];
      inherit gateReuse;
    };
    # The lookup's units only: burrow-events would start the full pass,
    # which lists repos on GitHub.
    burrow.events.enable = lib.mkForce false;
    burrow.events.host = "example.org";
    services.nginx = {
      enable = true;
      virtualHosts."example.org" = {
        onlySSL = true;
        sslCertificate = "${tls}/cert.pem";
        sslCertificateKey = "${tls}/key.pem";
      };
    };
    networking.hosts."127.0.0.1" = [ "example.org" ];
    users.users.dev.isNormalUser = true;
  };
in
pkgs.testers.runNixOSTest {
  name = "ledger-gate-reuse";

  nodes.on = machine { enable = true; repos = [ "sam/homelab" ]; path = "/conary/gate-reuse"; };
  nodes.off = machine { };

  testScript = { nodes, ... }: ''
    import json

    # A ledger set up with no ops yet, as `cn init` leaves one: its
    # anchor's copy, then its public key (RFC 8032's first), which starts
    # both conaryds.
    key = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
    for m in [on, off]:
        m.wait_for_unit("multi-user.target")
        m.succeed("echo none >/var/lib/burrow/conary/anchor")
        m.succeed(f"echo {key} >/var/lib/burrow/conary/key.pub")
        m.wait_for_unit("conaryd.service")

    def status(m, url):
        return m.succeed(f"curl -s -o /dev/null -w '%{{http_code}}' --cacert ${tls}/cert.pem -X POST -H 'Content-Type: application/json' -d '{{}}' {url}")

    with subtest("on, an unsigned request is refused over loopback"):
        on.wait_for_unit("conary-gate-reuse.service")
        on.wait_for_open_port(8091, "127.0.0.1")
        assert status(on, "http://127.0.0.1:8091/") in ("400", "401")

    with subtest("and over the vhost's HTTPS path"):
        on.wait_for_unit("nginx.service")
        assert status(on, "${nodes.on.burrow.ledger.gateReuse.url}") in ("400", "401")
        assert "${nodes.on.burrow.ledger.gateReuse.url}" == "https://example.org/conary/gate-reuse"

    with subtest("its signer is the train's, and it has the ledger's token"):
        env = on.succeed("systemctl show -P Environment conary-gate-reuse.service").split()
        path = next(e for e in env if e.startswith("CONARY_GATE_REUSE_CONFIG=")).split("=", 1)[1]
        config = json.loads(on.succeed(f"cat {path}"))
        assert config["signer"] == "${nodes.on.burrow.ledger.signerWorkflow}", config
        assert config["fresh"] is None and config["repos"] == ["sam/homelab"], config
        token = on.succeed(f"su dev -s /bin/sh -c '{config['token_command']} sam/homelab'")
        assert token == "gho_stand_in\n", token

    with subtest("it runs as the user, and the socket's conaryd keeps no network"):
        on.succeed("systemctl show -P User conary-gate-reuse.service | grep -x dev")
        on.succeed("systemctl show -P PrivateNetwork conaryd.service | grep -x yes")
        on.succeed("test -S /run/conary/conaryd.sock")

    with subtest("off, nothing listens"):
        off.fail("systemctl cat conary-gate-reuse.service")
        off.fail("ss -Htln | grep -w 8091")
        off.succeed("test -S /run/conary/conaryd.sock")
  '';
}
