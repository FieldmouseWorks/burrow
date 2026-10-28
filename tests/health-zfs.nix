# HOST-health's ZFS check, run against a stand-in zpool: on the example
# machine with a second pool mounted too, a backup pool only noauto mounts use
# and an extra pool by GUID, it checks the pools the machine needs (rpool,
# tank, the GUID's) and only those. A missing or unhealthy pool fails, a GUID
# names whichever pool has it and no other, and zpool failing fails, whatever
# it printed first. A machine with ZFS support and no pool it needs has no ZFS
# check.
#
#   nix build .#checks.x86_64-linux.health-zfs -L
{ self, pkgs }:
let
  inherit (pkgs) lib;
  base = self.nixosConfigurations.example;
  pools = base.extendModules {
    modules = [{
      fileSystems."/srv" = { device = "tank/srv"; fsType = "zfs"; };
      # tank is needed for /srv, whatever its noauto mount says.
      fileSystems."/tank/archive" = { device = "tank/archive"; fsType = "zfs"; options = [ "noauto" ]; };
      fileSystems."/backup" = { device = "backup/data"; fsType = "zfs"; options = [ "noauto" ]; };
      boot.zfs.extraPools = [ "12345" "rpool" ];
    }];
  };
  noPools = base.extendModules {
    modules = [{
      fileSystems."/" = lib.mkForce { device = "/dev/sda1"; fsType = "ext4"; };
      fileSystems."/backup" = { device = "backup/data"; fsType = "zfs"; options = [ "noauto" ]; };
    }];
  };

  # zpool, as far as the check can tell: the pools in $POOLS, a line each of
  # name, GUID and health. With $ZPOOL_FAIL naming the subcommand, that one
  # says all that and fails anyway.
  zpool = pkgs.writeShellScriptBin "zpool" ''
    case "$1" in
      list)
        while read -r name guid _; do printf '%s\t%s\n' "$guid" "$name"; done <"$POOLS" ;;
      status)
        found=
        while read -r name _ health; do
          if [ "$name" = "$3" ]; then
            if [ "$health" = ONLINE ]; then echo "pool '$name' is healthy"
            else printf '  pool: %s\n state: %s\n' "$name" "$health"; fi
            found=1
          fi
        done <"$POOLS"
        [ -n "$found" ] || { echo "cannot open '$3': no such pool" >&2; exit 1; } ;;
    esac
    [ "''${ZPOOL_FAIL:-}" != "$1" ]
  '';
  check = pkgs.writeShellScript "health-zfs" ''
    export PATH=${lib.makeBinPath [ zpool pkgs.coreutils pkgs.gnugrep ]}
    ${pools.config.burrow.updates.health.zfs}
  '';
in
assert noPools.config.boot.zfs.enabled && !(noPools.config.burrow.updates.health ? zfs);
pkgs.runCommand "health-zfs-check" { } ''
  export POOLS=$PWD/pools
  # The check, on these pools: pass or fail, as expected.
  pools() { printf '%s\n' "$@" >pools; }
  passes() { ${check} || { echo "$1: failed"; exit 1; }; }
  fails() { ! ${check} || { echo "$1: passed"; exit 1; }; }

  pools "rpool 1 ONLINE" "tank 2 ONLINE" "extra 12345 ONLINE"
  passes "all healthy, the backup pool away"
  pools "rpool 1 ONLINE" "tank 2 ONLINE" "spare 12345 ONLINE"
  passes "the GUID's pool by another name"

  pools "rpool 1 ONLINE" "tank 2 DEGRADED" "extra 12345 ONLINE"
  fails "a degraded pool"
  pools "tank 2 ONLINE" "extra 12345 ONLINE"
  fails "the root pool missing"
  pools "rpool 1 ONLINE" "extra 12345 ONLINE" "backup 3 ONLINE"
  fails "a pool shared with a noauto mount missing"
  pools "rpool 1 ONLINE" "tank 2 ONLINE"
  fails "the GUID's pool missing"
  pools "rpool 1 ONLINE" "tank 2 ONLINE" "extra 999 ONLINE"
  fails "another pool with the GUID's pool's name"
  pools "rpool 1 ONLINE" "tank 2 ONLINE" "extra 12345 FAULTED"
  fails "the GUID's pool faulted"
  pools "rpool 1 ONLINE" "tank 2 ONLINE" "extra 12345 ONLINE"
  ZPOOL_FAIL=status fails "zpool status failing after saying healthy"
  ZPOOL_FAIL=list fails "zpool list failing after listing the GUID"
  rm pools
  fails "zpool failing"

  touch $out
''
