# robot as the example machine has it: the config built into it reads
# (a field the binary doesn't know fails it), a call to a webservice
# that isn't there says which call failed and nothing of the
# credentials, and a call it can't read asks nothing. Its calls are
# crates/burrow-robot's tests.
{ self, pkgs, ... }:
let
  robot = pkgs.lib.getExe self.nixosConfigurations.example.config.burrow.tools.robot;
in pkgs.runCommand "robot-config-check" { } ''
  code=0
  ROBOT_USER=sam ROBOT_PASSWORD=hunter2 ROBOT_URL=http://127.0.0.1:9 ${robot} wol 2>err || code=$?
  [ "$code" = 1 ] || { echo "wol: exit $code: $(cat err)"; exit 1; }
  grep -q '^robot: POST /wol/203.0.113.7: ' err || { echo "wol said: $(cat err)"; exit 1; }
  if grep -q hunter2 err; then echo "the password in: $(cat err)"; exit 1; fi
  code=0
  ${robot} reset cold 2>/dev/null || code=$?
  [ "$code" = 2 ] || { echo "reset cold: exit $code"; exit 1; }
  touch $out
''
