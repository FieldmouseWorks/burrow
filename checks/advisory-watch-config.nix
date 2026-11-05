# advisory-watch as the example machine runs it: the config built into
# it reads (a field the binary doesn't know fails it), and a report,
# which only reads, has nothing yet. Its rules are
# crates/advisory-watch's tests.
{ self, pkgs, ... }:
let
  watch = self.nixosConfigurations.example.config.systemd.services.advisory-watch.serviceConfig.ExecStart;
in pkgs.runCommand "advisory-watch-config-check" { } ''
  got=$(${watch} report 2>err) || { echo "advisory-watch report failed (exit $?): $(cat err)"; exit 1; }
  [ "$got" = "nothing seen yet" ] || { echo "advisory-watch report said: $got"; exit 1; }
  touch $out
''
