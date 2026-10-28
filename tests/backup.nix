# HOST-backup for real, on a ZFS pool, to a restic repository behind rclone
# (a local remote standing in for Google Drive): it backs up the snapshot, not
# the live files, leaves out what it should, sends only what changed, holds
# the snapshot it read until the next one is off the box, and keeps nothing
# readable without the password. The drill restores and compares, and fails
# when the backup isn't what the snapshot had or the repository is damaged;
# a stale snapshot fails the pass and tells the owner.
#
#   nix build .#checks.x86_64-linux.backup -L
{ self, pkgs }:
pkgs.testers.runNixOSTest {
  name = "backup";

  nodes.machine = { pkgs, ... }: {
    imports = [ self.nixosModules.backup ];
    environment.systemPackages = [ pkgs.restic pkgs.jq ];
    virtualisation.emptyDiskImages = [ 2048 ];
    virtualisation.memorySize = 2048;
    boot.supportedFilesystems = [ "zfs" ];
    networking.hostId = "8425e349";
    users.users.dev.isNormalUser = true;

    environment.etc."backup-password".text = "correct horse battery staple";
    environment.etc."backup-rclone.conf".text = ''
      [offsite]
      type = local
    '';
    burrow.backup = {
      enable = true;
      repository = "rclone:offsite:/var/lib/offsite/repo";
      passwordFile = "/etc/backup-password";
      rcloneConfigFile = "/etc/backup-rclone.conf";
      datasets = [ "tank/data" "tank/keep" ];
      snapshots = "snap-*";
      exclude = [ "/data/skip" ];
      drill = [ "/data/dev/src/proj" "/srv/keep" ];
      verifyDays = 1;
      # Nothing forgotten, so the day's prune has nothing to repack and the
      # check is what reads a damaged pack.
      keep.last = 100;
    };
  };

  testScript = ''
    import json

    def backups():
        return json.loads(machine.succeed("machine-backup restic snapshots --json"))

    def files(snap="latest"):
        return machine.succeed(f"machine-backup restic ls {snap} --json | jq -r 'select(.message_type == \"node\") | .path'").split()

    def holds(snap):
        return machine.succeed(f"zfs holds -H tank/data@{snap} tank/keep@{snap} | cut -f2 | sort -u").split()

    def snapshot(name):
        machine.succeed(f"zfs snapshot tank/data@{name} tank/keep@{name}")

    machine.wait_for_unit("multi-user.target")
    machine.succeed(
        "zpool create -O mountpoint=none -O compression=zstd tank /dev/vdb",
        "zfs create -o mountpoint=/data tank/data",
        "zfs create -o mountpoint=/srv/keep tank/keep",
        "mkdir -p /data/dev/src/proj/sub /data/skip /data/proj2/target /data/marked",
        "echo burrow-plaintext-marker >/data/dev/src/proj/a.txt",
        "head -c 48M /dev/urandom >/data/dev/src/proj/sub/big.bin",
        "ln -s a.txt /data/dev/src/proj/link",
        "chown 1000:100 /data/dev/src/proj/a.txt && chmod 640 /data/dev/src/proj/a.txt",
        "echo left-out >/data/skip/x",
        "printf 'Signature: 8a477f597d28d172789f06886806bc55\\n' >/data/proj2/target/CACHEDIR.TAG",
        "echo build >/data/proj2/target/out",
        "echo kept >/data/proj2/main.rs",
        "touch /data/marked/.nobackup && echo left-out >/data/marked/x",
        "echo keep >/srv/keep/k.txt",
    )

    with subtest("no snapshot to back up fails the pass and tells the owner"):
        machine.fail("systemctl start machine-backup")
        machine.succeed("journalctl -t machine-notify | grep 'open backup'")

    with subtest("the first pass creates the repository and backs up the snapshot, not the live files"):
        snapshot("snap-1")
        machine.succeed("echo too-late >/data/dev/src/proj/after.txt")
        machine.succeed("systemctl start machine-backup")
        [b] = backups()
        assert b["tags"] == ["snap-1"], b
        assert sorted(b["paths"]) == ["/data", "/srv/keep"], b
        got = files()
        for want in ["/data/dev/src/proj/a.txt", "/data/dev/src/proj/sub/big.bin", "/data/dev/src/proj/link", "/data/proj2/main.rs", "/srv/keep/k.txt"]:
            assert want in got, (want, got)
        for unwanted in ["/data/dev/src/proj/after.txt", "/data/skip/x", "/data/proj2/target/out", "/data/marked/x"]:
            assert unwanted not in got, (unwanted, got)
        assert holds("snap-1") == ["burrow-backup"], holds("snap-1")
        # The live paths are as they were: the snapshot was mounted only for restic.
        machine.succeed("test -e /data/dev/src/proj/after.txt")
        machine.succeed("journalctl -t machine-notify | grep 'close backup'")
        # The day's upkeep ran, drill included.
        machine.succeed("test -e /var/lib/burrow/backup/maintained")
        machine.succeed("journalctl -u machine-backup | grep 'drill: /data/dev/src/proj from backup .* is what snap-1 had'")

    with subtest("nothing is readable without the password"):
        machine.fail("grep -r burrow-plaintext-marker /var/lib/offsite")
        machine.fail("RESTIC_PASSWORD=wrong restic -r /var/lib/offsite/repo snapshots")

    with subtest("a pass with no new snapshot sends nothing"):
        machine.succeed("systemctl start machine-backup")
        assert len(backups()) == 1

    with subtest("the next snapshot sends only what changed, and the hold moves to it"):
        machine.succeed("printf X | dd of=/data/dev/src/proj/sub/big.bin bs=1 seek=25000000 conv=notrunc")
        snapshot("snap-2")
        machine.succeed("systemctl start machine-backup")
        assert [b["tags"] for b in backups()] == [["snap-1"], ["snap-2"]]
        last = json.loads(machine.succeed("cat /var/lib/burrow/backup/last.json"))
        assert 0 < last["added"] < 24 * 2**20, last
        assert holds("snap-2") == ["burrow-backup"], holds("snap-2")
        assert holds("snap-1") == [], holds("snap-1")

    with subtest("the drill brings back what the snapshot had"):
        machine.succeed("machine-backup drill")
        machine.succeed("machine-backup drill /data")

    with subtest("the drill fails when the backup isn't what its snapshot had"):
        machine.succeed(
            "zfs release burrow-backup tank/data@snap-2",
            "zfs destroy tank/data@snap-2",
            "echo changed >/data/dev/src/proj/a.txt",
            "zfs snapshot tank/data@snap-2",
        )
        out = machine.fail("machine-backup drill 2>&1")
        assert "isn't what snap-2 had" in out, out
        machine.succeed("chmod 600 /data/dev/src/proj/a.txt && echo burrow-plaintext-marker >/data/dev/src/proj/a.txt")
        machine.succeed("zfs destroy tank/data@snap-2 && zfs snapshot tank/data@snap-2")
        out = machine.fail("machine-backup drill 2>&1")
        assert "metadata" in out, out

    with subtest("a damaged repository fails the day's check"):
        machine.succeed("""
          pack=$(find /var/lib/offsite/repo/data -type f | head -1)
          printf garbage | dd of="$pack" bs=1 seek=100 conv=notrunc
        """)
        machine.succeed("rm /var/lib/burrow/backup/maintained")
        snapshot("snap-3")
        machine.fail("systemctl start machine-backup")
        machine.succeed("journalctl -u machine-backup | grep 'restic check found damage'")
        machine.succeed("journalctl -t machine-notify | grep 'open backup'")

    with subtest("a snapshot older than staleAfter fails the pass"):
        machine.succeed("date -s '+7 hours'")
        machine.fail("systemctl start machine-backup")
        machine.succeed("journalctl -u machine-backup | grep 'is more than 6 hours old'")
  '';
}
