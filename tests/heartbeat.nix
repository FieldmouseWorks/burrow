# heartbeat's beats, for real: the example machine's own beat, with a stand-in
# for HOST-health and a local stand-in for Healthchecks, checks that a healthy
# machine pings success with its skipped checks left out, that a check failing
# one beat is only logged (a failed unit riding along), that one failing two
# beats running pings fail and names it, that passing again clears its streak,
# that the ping key is read trimmed from the unit's credential, and that a ping
# Healthchecks refuses is reported but never fails the beat (a failed unit
# would make HOST-update's health compare revert a switch).
#
#   nix build .#checks.x86_64-linux.heartbeat -L
{ self, pkgs }:
let
  inherit (pkgs) lib;
  port = 18093;

  # HOST-health, as far as the beat can tell: the lines in $HEALTH_REPORT.
  health = pkgs.writeShellScriptBin "burrow-health" ''cat "$HEALTH_REPORT"'';
  machine = self.nixosConfigurations.example.extendModules {
    modules = [{
      burrow.tools.health = lib.mkForce health;
      burrow.heartbeat.url = "http://127.0.0.1:${toString port}";
    }];
  };
  inherit (machine.config.burrow.heartbeat) check;
  beat = machine.config.systemd.services."${machine.config.burrow.prefix}-heartbeat".serviceConfig.ExecStart;

  # Healthchecks, as far as the beat can tell: takes pings for its one check
  # (KEY and the slug), records each one, 404s the rest.
  server = pkgs.writeText "healthchecks.py" ''
    import http.server, json, sys
    port, log, prefix = int(sys.argv[1]), sys.argv[2], sys.argv[3]

    class Ping(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            body = self.rfile.read(int(self.headers.get("Content-Length", 0))).decode()
            with open(log, "a") as f:
                f.write(json.dumps({"path": self.path, "body": body}) + "\n")
            ok = self.path == prefix or self.path.startswith(prefix + "/")
            self.send_response(200 if ok else 404)
            self.end_headers()
            self.wfile.write(b"OK" if ok else b"not found")

        def log_message(self, *args):
            pass

    http.server.HTTPServer(("127.0.0.1", port), Ping).serve_forever()
  '';
in
pkgs.runCommand "heartbeat-check" { nativeBuildInputs = [ pkgs.python3 pkgs.jq pkgs.curl ]; } ''
  mkdir creds run
  printf ' KEY \n' >creds/ping-key
  export CREDENTIALS_DIRECTORY=$PWD/creds RUNTIME_DIRECTORY=$PWD/run HEALTH_REPORT=$PWD/report
  python3 -I ${server} ${toString port} $PWD/pings /KEY/${check} &
  for _ in $(seq 50); do curl -s -o /dev/null http://127.0.0.1:${toString port}/ -d "" && break; sleep 0.1; done
  : >pings

  # One beat with these health lines; then what it pinged.
  beat() { printf '%s\n' "$@" >report; ${beat} >/dev/null; }
  pinged() { tail -n1 pings | jq -r "$1"; }
  expect() { [ "$(pinged .path)" = "/KEY/${check}$1" ] || { echo "$3: pinged $(pinged .path)"; exit 1; }
             pinged .body | grep -qxF "$2" || { echo "$3: said $(pinged .body)"; exit 1; }; }

  beat "check:sshd ok" "check:github fail" "check:events ok"
  expect "" "healthy: 2 checks pass" "healthy, github skipped"

  beat "check:sshd ok" "check:events fail" "unit:autoland.service fail"
  expect /log "failing, not for long yet: events" "one bad beat"
  expect /log "failed units (not counted): autoland.service" "one bad beat's units"

  beat "check:sshd ok" "check:events fail" "check:zfs fail"
  expect /fail "failing for 2 beats or more: events" "two bad beats"
  expect /fail "failing for less: zfs" "two bad beats, and a new one"

  beat "check:sshd ok" "check:events ok" "check:zfs ok"
  expect "" "healthy: 3 checks pass" "recovered"
  beat "check:sshd ok" "check:events fail" "check:zfs ok"
  expect /log "failing, not for long yet: events" "a streak starts over"

  printf 'NOTKEY\n' >creds/ping-key
  ${beat} 2>err >/dev/null || { echo "a refused ping failed the beat"; exit 1; }
  grep -q "didn't land" err || { echo "a refused ping went unreported: $(cat err)"; exit 1; }

  touch $out
''
