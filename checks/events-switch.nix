# A switch that changes a handler's units kicks that handler's full
# pass, and only its: the switch restarts an active unit that changed,
# so the example's burrow-events-switched-autoland must change with
# autoland's units (either of them) and stay active once started,
# renovate's must not change, and its start must kick autoland, but
# not at boot.
{ self, pkgs, ... }:
let
  base = self.nixosConfigurations.example;
  moved = svc: base.extendModules { modules = [{ systemd.services.${svc}.environment.MOVED = "1"; }]; };
  unit = c: h: let n = "burrow-events-switched-${h}.service"; in "${c.config.systemd.units.${n}.unit}/${n}";
  start = base.config.systemd.services.burrow-events-switched-autoland.serviceConfig.ExecStart;
in pkgs.runCommand "events-switch-check" { } ''
  differ() { ! cmp -s "$1" "$2" || { echo "$3"; exit 1; }; }
  differ ${unit base "autoland"} ${unit (moved "autoland") "autoland"} "autoland.service changed, autoland's didn't"
  differ ${unit base "autoland"} ${unit (moved "autoland@") "autoland"} "autoland@.service changed, autoland's didn't"
  cmp -s ${unit base "renovate"} ${unit (moved "autoland") "renovate"} || { echo "autoland changed renovate's"; exit 1; }
  grep -qx RemainAfterExit=true ${unit base "autoland"} || { echo "not active once started"; exit 1; }
  grep -qx WantedBy=multi-user.target ${unit base "autoland"} || { echo "not started"; exit 1; }

  # Its start, with stand-ins for systemctl and curl.
  mkdir bin
  printf '#!${pkgs.runtimeShell}\necho "$STATE"\n' >bin/systemctl
  printf '#!${pkgs.runtimeShell}\necho "$*" >>%s/kicks\n' "$PWD" >bin/curl
  chmod +x bin/*
  PATH=$PWD/bin:$PATH STATE=starting ${start}
  [ ! -e kicks ] || { echo "kicked at boot: $(cat kicks)"; exit 1; }
  PATH=$PWD/bin:$PATH STATE=running ${start}
  grep -q ' http://127.0.0.1:[0-9]*/kick/autoland$' kicks || { echo "kicked: $(cat kicks)"; exit 1; }
  touch $out
''
