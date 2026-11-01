# heartbeat's beat as the example machine builds it: its own HOST-heartbeat,
# with a stand-in for HOST-health and nothing listening where the pings go.
# It reads the config its module builds in and says the beat (a skipped check
# left out), and a ping that doesn't land is said, without the key, but never
# fails the beat (a failed unit would make HOST-update's health compare revert
# a switch). The beats themselves, and the pings against a stand-in
# Healthchecks, are crates/burrow-heartbeat's tests.
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
    }];
  };
  inherit (machine.config.burrow.heartbeat) check;
  beat = machine.config.systemd.services."${machine.config.burrow.prefix}-heartbeat".serviceConfig.ExecStart;
in
pkgs.runCommand "heartbeat-check" { } ''
  mkdir creds run
  printf ' KEY \n' >creds/ping-key
  export CREDENTIALS_DIRECTORY=$PWD/creds RUNTIME_DIRECTORY=$PWD/run
  ${beat} >said 2>err || { echo "a ping that didn't land failed the beat: $(cat err)"; exit 1; }
  [ "$(cat said)" = "/ healthy: 2 checks pass" ] || { echo "the beat said: $(cat said)"; exit 1; }
  grep -q "^the ping to ${check} didn't land (" err || { echo "the lost ping went unsaid: $(cat err)"; exit 1; }
  ! grep -q KEY err || { echo "the key reached the journal: $(cat err)"; exit 1; }
  touch $out
''
