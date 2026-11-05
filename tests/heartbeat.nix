# heartbeat's beat as the example machine builds it: its own HOST-heartbeat,
# with a stand-in for HOST-health and nothing listening where the pings go.
# It reads the config its module builds in and says the beat (a skipped check
# left out), and a ping that doesn't land is said, without the key, but never
# fails the beat (a failed unit would make HOST-update's health compare revert
# a switch). The beats themselves, and the pings against a stand-in
# Healthchecks, are crates/burrow-heartbeat's tests.
#
# A watched site's unit, too, against a local web server: a site that
# answers 2xx is pinged (and a ping that doesn't land is said, without the
# key), a redirect or a site that doesn't answer isn't, and none fails the
# unit. What counts as an answer, and the pings against a stand-in
# Healthchecks, are crates/burrow-heartbeat's tests/site.rs.
#
#   nix build .#checks.x86_64-linux.heartbeat -L
{ self, pkgs }:
let
  inherit (pkgs) lib;

  # HOST-health, as far as the beat can tell.
  health = pkgs.writeShellScriptBin "burrow-health" ''
    printf '%s\n' "check:sshd ok" "check:github fail" "check:events ok"
  '';
  machine = self.nixosConfigurations.example.extendModules {
    modules = [{
      burrow.tools.health = lib.mkForce health;
      # The discard port: nothing answers it.
      burrow.heartbeat.url = "http://127.0.0.1:9";
      burrow.heartbeat.sites = {
        up = "http://127.0.0.1:8000/health";
        # busybox httpd sends a directory asked for without its slash on.
        moved = "http://127.0.0.1:8000/dir";
        down = "http://127.0.0.1:9/health";
      };
    }];
  };
  inherit (machine.config.burrow.heartbeat) check;
  beat = machine.config.systemd.services."${machine.config.burrow.prefix}-heartbeat".serviceConfig.ExecStart;
  site = slug: machine.config.systemd.services."${machine.config.burrow.prefix}-heartbeat-site-${slug}".serviceConfig.ExecStart;
in
pkgs.runCommand "heartbeat-check" { } ''
  mkdir creds run
  printf ' KEY \n' >creds/ping-key
  export CREDENTIALS_DIRECTORY=$PWD/creds RUNTIME_DIRECTORY=$PWD/run
  ${beat} >said 2>err || { echo "a ping that didn't land failed the beat: $(cat err)"; exit 1; }
  [ "$(cat said)" = "/ healthy: 2 checks pass" ] || { echo "the beat said: $(cat said)"; exit 1; }
  grep -q "^the ping to ${check} didn't land (" err || { echo "the lost ping went unsaid: $(cat err)"; exit 1; }
  ! grep -q KEY err || { echo "the key reached the journal: $(cat err)"; exit 1; }

  mkdir -p www/dir
  echo ok >www/health
  ${pkgs.busybox}/bin/httpd -f -p 127.0.0.1:8000 -h www &
  httpd=$!
  for _ in $(seq 50); do
    ${lib.getExe pkgs.curl} -fs -o /dev/null http://127.0.0.1:8000/health && break
    sleep 0.1
  done
  ${site "up"} >said 2>err || { echo "a site's lost ping failed its unit: $(cat err)"; exit 1; }
  [ "$(cat said)" = "up: http://127.0.0.1:8000/health answers HTTP 200" ] || { echo "the up site said: $(cat said) $(cat err)"; exit 1; }
  grep -q "^up: the ping to up didn't land (" err || { echo "the up site's lost ping went unsaid: $(cat said) $(cat err)"; exit 1; }
  ! grep -q KEY said err || { echo "the key reached the journal: $(cat said err)"; exit 1; }
  ${site "moved"} >said 2>err || { echo "a redirect failed its unit: $(cat err)"; exit 1; }
  grep -q "^moved: http://127.0.0.1:8000/dir didn't answer 2xx (HTTP 30[0-9]), so no ping$" said || { echo "the moved site said: $(cat said) $(cat err)"; exit 1; }
  [ ! -s err ] || { echo "the moved site pinged: $(cat err)"; exit 1; }
  ${site "down"} >said 2>err || { echo "a site that's down failed its unit: $(cat err)"; exit 1; }
  grep -q "^down: http://127.0.0.1:9/health didn't answer 2xx (.*), so no ping$" said || { echo "the down site said: $(cat said) $(cat err)"; exit 1; }
  [ ! -s err ] || { echo "the down site pinged: $(cat err)"; exit 1; }
  kill $httpd
  touch $out
''
