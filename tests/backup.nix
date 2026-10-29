# HOST-backup for real, on a ZFS pool, to a restic repository behind rclone
# (a local remote standing in for Google Drive): it backs up the snapshot, not
# the live files, nested datasets included whatever order they're listed in;
# leaves out what it should; sends only what changed; holds the snapshot it
# read until the next one is off the box, and lets go of any other; keeps
# rclone's refreshed config; and keeps nothing readable without the password.
# The drill restores and compares directories, files, symlinks and paths with
# spaces, and fails when the backup is missing something its snapshot had or
# a restore isn't what it should be, without printing what's in the files,
# and passes when a remount moves the snapshot to another device. A damaged
# share of the repository stays reported until it reads back whole, and a
# pass that can't start, fails along the way or finds a stale snapshot tells
# the owner.
#
#   nix build .#checks.x86_64-linux.backup -L
{ self, pkgs }:
pkgs.testers.runNixOSTest {
  name = "backup";

  nodes.machine = { pkgs, ... }: {
    imports = [ self.nixosModules.backup ];
    environment.systemPackages = [ pkgs.restic pkgs.jq pkgs.e2fsprogs ];
    virtualisation.emptyDiskImages = [ 2048 ];
    virtualisation.memorySize = 2048;
    boot.supportedFilesystems = [ "zfs" ];
    boot.zfs.forceImportRoot = false;
    networking.hostId = "8425e349";
    users.users.dev.isNormalUser = true;

    environment.etc."backup-password".text = "correct horse battery staple";
    burrow.backup = {
      enable = true;
      repository = "rclone:offsite:/var/lib/offsite/repo";
      passwordFile = "/etc/backup-password";
      rcloneConfigFile = "/var/lib/rclone-secret.conf";
      # A child before its parent: mounting /data mustn't hide /data/nested.
      # tank/keep's mountpoint has a space in it, which findmnt's plain output
      # escapes.
      datasets = [ "tank/data/nested" "tank/data" "tank/keep" ];
      snapshots = "snap-*";
      exclude = [ "/data/skip" ];
      drill = [ "/data/dev/src/proj" "/srv/keep it" "/data/with space" ];
      # Two shares, read back in turn.
      verifyDays = 2;
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
        return machine.succeed(f"machine-backup restic ls {snap} --json | jq -r 'select(.message_type == \"node\") | .path'").splitlines()

    datasets = ["tank/data/nested", "tank/data", "tank/keep"]

    def holds(snap):
        return machine.succeed(f"zfs holds -H {' '.join(d + '@' + snap for d in datasets)} | cut -f2 | sort -u").split()

    def snapshot(name):
        machine.succeed(f"zfs snapshot {' '.join(d + '@' + name for d in datasets)}")

    def notices():
        return int(machine.succeed("journalctl -t machine-notify | grep -c 'open backup' || true"))

    def told_owner(before):
        machine.wait_until_succeeds(f"[ $(journalctl -t machine-notify | grep -c 'open backup') -gt {before} ]")

    machine.wait_for_unit("multi-user.target")
    # Passes run when the test says, not when a clock jump says.
    machine.succeed("systemctl stop machine-backup.timer")
    machine.succeed(
        "printf '[offsite]\\ntype = local\\n' >/var/lib/rclone-secret.conf",
        "zpool create -O mountpoint=none -O compression=zstd tank /dev/vdb",
        "zfs create -o mountpoint=/data tank/data",
        "zfs create -o mountpoint=/data/nested tank/data/nested",
        "zfs create -o 'mountpoint=/srv/keep it' tank/keep",
        "mkdir -p /data/dev/src/proj/sub /data/skip /data/proj2/target /data/marked '/data/with space'",
        "echo burrow-plaintext-marker >/data/dev/src/proj/a.txt",
        "head -c 48M /dev/urandom >/data/dev/src/proj/sub/big.bin",
        "ln -s a.txt /data/dev/src/proj/link",
        "ln -s /nowhere /data/dev/src/proj/dangling",
        "chown 1000:100 /data/dev/src/proj/a.txt && chmod 640 /data/dev/src/proj/a.txt",
        "echo spaced >'/data/with space/f'",
        "echo nested >/data/nested/n.txt",
        "echo linked >/data/nested/h1 && ln /data/nested/h1 /data/nested/h2",
        "ln -s nested/n.txt /data/toplink",
        "echo left-out >/data/skip/x",
        "printf 'Signature: 8a477f597d28d172789f06886806bc55\\n' >/data/proj2/target/CACHEDIR.TAG",
        "echo build >/data/proj2/target/out",
        "echo kept >/data/proj2/main.rs",
        "touch /data/marked/.nobackup && echo left-out >/data/marked/x",
        "echo keep >'/srv/keep it/k.txt'",
    )

    with subtest("no snapshot to back up fails the pass and tells the owner"):
        # Snapshots of the datasets one by one, each under its own name, as
        # sanoid's recursive = true takes them, are none to back up.
        for i, d in enumerate(datasets):
            machine.succeed(f"zfs snapshot {d}@snap-0{i}")
        machine.fail("systemctl start machine-backup")
        machine.succeed("journalctl -u machine-backup | grep 'that all of .* have under one name'")
        machine.succeed("journalctl -t machine-notify | grep 'open backup'")
        for i, d in enumerate(datasets):
            machine.succeed(f"zfs destroy {d}@snap-0{i}")

    with subtest("the first pass creates the repository and backs up the snapshot, not the live files"):
        snapshot("snap-1")
        machine.succeed("echo too-late >/data/dev/src/proj/after.txt")
        machine.succeed("systemctl start machine-backup")
        [b] = backups()
        assert b["tags"] == ["snap-1"], b
        assert sorted(b["paths"]) == ["/data", "/data/nested", "/srv/keep it"], b
        got = files()
        for want in ["/data/dev/src/proj/a.txt", "/data/dev/src/proj/sub/big.bin", "/data/dev/src/proj/link",
                     "/data/dev/src/proj/dangling", "/data/proj2/main.rs", "/data/nested/n.txt", "/data/nested/h2",
                     "/data/toplink", "/data/with space/f", "/srv/keep it/k.txt"]:
            assert want in got, (want, got)
        for unwanted in ["/data/dev/src/proj/after.txt", "/data/skip/x", "/data/proj2/target/out", "/data/marked/x"]:
            assert unwanted not in got, (unwanted, got)
        assert holds("snap-1") == ["burrow-backup"], holds("snap-1")
        # The live paths are as they were: the snapshot was mounted only for restic.
        machine.succeed("test -e /data/dev/src/proj/after.txt")
        machine.succeed("journalctl -t machine-notify | grep 'close backup'")
        # The day's upkeep ran, drill included.
        machine.succeed("test -e /var/lib/burrow/backup/maintained")
        journal = machine.succeed("journalctl -u machine-backup")
        for want in ["holds all of snap-1", "drill: /data/dev/src/proj is what the snapshot had", "drill: /srv/keep it is what", "drill: /data/with space is what"]:
            assert want in journal, (want, journal)

    with subtest("nothing is readable without the password"):
        machine.fail("grep -r burrow-plaintext-marker /var/lib/offsite")
        machine.fail("RESTIC_PASSWORD=wrong restic -r /var/lib/offsite/repo snapshots")

    with subtest("a pass with no new snapshot sends nothing"):
        machine.succeed("systemctl start machine-backup")
        assert len(backups()) == 1

    with subtest("rclone's refreshed config is kept until its secret changes"):
        machine.succeed("echo '# refreshed' >>/var/lib/burrow/backup/rclone.conf")
        machine.succeed("systemctl start machine-backup")
        machine.succeed("grep -x '# refreshed' /var/lib/burrow/backup/rclone.conf")
        machine.succeed("echo '# rotated' >>/var/lib/rclone-secret.conf")
        machine.succeed("systemctl start machine-backup")
        machine.succeed("cmp /var/lib/rclone-secret.conf /var/lib/burrow/backup/rclone.conf")

    with subtest("a pass that can't start tells the owner"):
        n = notices()
        machine.succeed("mv /var/lib/rclone-secret.conf /var/lib/rclone-secret.away")
        machine.fail("systemctl start machine-backup")
        machine.succeed("mv /var/lib/rclone-secret.away /var/lib/rclone-secret.conf")
        machine.succeed("journalctl -u machine-backup | grep \"can't read rclone's config\"")
        machine.succeed("test -e /var/lib/burrow/backup/failed-since")
        told_owner(n)

    with subtest("so does whatever else stops a pass"):
        n = notices()
        machine.succeed("chattr +i /var/lib/burrow/backup")
        machine.fail("systemctl start machine-backup")
        machine.succeed("chattr -i /var/lib/burrow/backup")
        machine.succeed("journalctl -u machine-backup | grep -F 'chmod 700 \"$state\" exited 1'")
        told_owner(n)

    with subtest("the next snapshot sends only what changed, and the hold moves to it"):
        machine.succeed("printf X | dd of=/data/dev/src/proj/sub/big.bin bs=1 seek=25000000 conv=notrunc")
        snapshot("snap-2")
        machine.succeed("systemctl start machine-backup")
        assert [b["tags"] for b in backups()] == [["snap-1"], ["snap-2"]]
        last = json.loads(machine.succeed("cat /var/lib/burrow/backup/last.json"))
        assert 0 < last["added"] < 24 * 2**20, last
        assert holds("snap-2") == ["burrow-backup"], holds("snap-2")
        assert holds("snap-1") == [], holds("snap-1")

    with subtest("a hold left by a run that died is let go"):
        machine.succeed("zfs hold burrow-backup tank/data@snap-1")
        machine.succeed("systemctl start machine-backup")
        assert holds("snap-1") == [], holds("snap-1")
        assert holds("snap-2") == ["burrow-backup"], holds("snap-2")

    with subtest("the drill brings back what the snapshot had: directories, files, symlinks"):
        machine.succeed("machine-backup drill")
        out = machine.succeed("machine-backup drill /data/dev/src/proj/a.txt /data/dev/src/proj/link /data/dev/src/proj/dangling /data 2>&1")
        assert out.count("is what the snapshot had") == 4, out

    with subtest("the drill passes when the snapshot comes back on another device"):
        # Restic keeps the device of hardlinked files, and a mount takes the
        # lowest free device number: a tmpfs mounted first moves the
        # snapshot's off the one it was backed up from.
        def device():
            return machine.succeed("unshare --mount --propagation private machine-backup __in-snapshot snap-2 stat -c %d /data/nested/h1").strip()
        before = device()
        machine.succeed("mkdir -p /mnt/shift && mount -t tmpfs shift /mnt/shift")
        assert device() != before, before
        machine.succeed("machine-backup drill")
        machine.succeed("umount /mnt/shift")

    with subtest("the drill forgets its own fresh backups and nothing else"):
        # Another machine's backup in the same repository, under the host name
        # a drill's fresh backups take, and a fresh backup a drill that died
        # before forgetting it left behind.
        machine.succeed("machine-backup restic backup -q --host machine-drill --tag drill /etc/machine-id")
        tag = "drill-" + "0" * 32
        machine.succeed(f"machine-backup restic backup -q --host machine-drill --tag {tag} /etc/machine-id")
        machine.succeed(f"echo {tag} >/var/lib/burrow/backup/probe")
        machine.succeed("machine-backup drill")
        left = [b["tags"] for b in backups() if b["hostname"] == "machine-drill"]
        assert left == [["drill"]], left
        machine.succeed("test ! -e /var/lib/burrow/backup/probe")

    with subtest("the comparison fails on a restore that's missing, changed or wrong"):
        # A restore made by hand, compared with the live files.
        machine.succeed(
            "mkdir -p /tmp/drill/tree/data/dev/src",
            "cp -a /data/dev/src/proj /tmp/drill/tree/data/dev/src/",
            "cd / && find /data/dev/src/proj -print0 >/tmp/drill/expected",
            "printf 'Signature: 8a477f597d28d172789f06886806bc55\\n' >/tmp/drill/CACHEDIR.TAG",
        )
        machine.succeed("machine-backup __compare /tmp/drill /data/dev/src/proj")
        # The same size and mtime, other bytes: and no bytes in the log.
        machine.succeed("cd /tmp/drill/tree/data/dev/src/proj && printf burrow-secret-text-xyz12 >a.txt && touch -r /data/dev/src/proj/a.txt a.txt")
        out = machine.fail("machine-backup __compare /tmp/drill /data/dev/src/proj 2>&1")
        assert "isn't what the snapshot had" in out and "burrow-secret" not in out and "plaintext" not in out, out
        machine.succeed("cp -a /data/dev/src/proj/a.txt /tmp/drill/tree/data/dev/src/proj/a.txt")
        machine.succeed("chmod 600 /tmp/drill/tree/data/dev/src/proj/a.txt")
        out = machine.fail("machine-backup __compare /tmp/drill /data/dev/src/proj 2>&1")
        assert "metadata" in out, out
        machine.succeed("chmod 640 /tmp/drill/tree/data/dev/src/proj/a.txt")
        machine.succeed("rm /tmp/drill/tree/data/dev/src/proj/link")
        out = machine.fail("machine-backup __compare /tmp/drill /data/dev/src/proj 2>&1")
        assert "< /data/dev/src/proj/link" in out, out

    with subtest("a damaged share of the repository stays reported until it reads back whole"):
        # Every backup kept and none rewritten yet, so the prune before the
        # check has nothing to repack and the check is what reads the pack.
        machine.succeed("cp -a /var/lib/offsite/repo /tmp/repo-good")
        pack = machine.succeed("find /var/lib/offsite/repo/data -type f | head -1").strip()
        # restic's share n/t holds the packs whose ID's first byte is n-1 mod t.
        part = int(pack.split("/")[-2], 16) % 2 + 1
        machine.succeed(f"printf garbage | dd of={pack} bs=1 seek=100 conv=notrunc")
        # The day's share is the damaged one; a damage record restic can't
        # take, from an upkeep cut short, is dropped rather than kept failing.
        machine.succeed(f"echo {part} >/var/lib/burrow/backup/check-next && rm /var/lib/burrow/backup/maintained")
        machine.succeed("echo 0/2 >/var/lib/burrow/backup/damaged")
        snapshot("snap-3")
        machine.fail("systemctl start machine-backup")
        machine.succeed(f"journalctl -u machine-backup | grep 'restic check found damage reading part {part}/2'")
        assert machine.succeed("cat /var/lib/burrow/backup/damaged").split() == [f"{part}/2"]
        machine.succeed("journalctl -t machine-notify | grep 'open backup'")
        # The next day backs up a new snapshot and reads the other share back
        # fine: the damage is still reported.
        machine.succeed("date -s '+1 day'")
        snapshot("snap-4")
        machine.fail("systemctl start machine-backup")
        assert backups()[-1]["tags"] == ["snap-4"], backups()
        machine.succeed(f"grep -x {part}/2 /var/lib/burrow/backup/damaged")
        # Read back whole, it's cleared.
        machine.succeed("rm -rf /var/lib/offsite/repo && mv /tmp/repo-good /var/lib/offsite/repo")
        machine.succeed("rm /var/lib/burrow/backup/maintained")
        machine.succeed("systemctl start machine-backup")
        machine.succeed("test ! -s /var/lib/burrow/backup/damaged && test ! -e /var/lib/burrow/backup/failed-since")
        # An empty cursor, as from a write cut short, starts at the first share.
        machine.succeed("truncate -s 0 /var/lib/burrow/backup/check-next && rm /var/lib/burrow/backup/maintained")
        machine.succeed("systemctl start machine-backup")
        assert machine.succeed("cat /var/lib/burrow/backup/check-next").strip() == "2"

    with subtest("the drill fails when the backup is missing what its snapshot had"):
        # A symlink restic reports nothing about, beside a dataset nested in
        # the same directory.
        machine.succeed("machine-backup restic rewrite --forget --host machine --exclude /data/toplink latest")
        out = machine.fail("machine-backup drill 2>&1")
        assert "doesn't hold all of snap-2" in out and "> /data/toplink" in out and "big.bin" not in out, out
        machine.succeed("machine-backup restic rewrite --forget --host machine --exclude /data/dev/src/proj/sub/big.bin latest")
        out = machine.fail("machine-backup drill 2>&1")
        assert "> /data/dev/src/proj/sub/big.bin" in out, out
        # The fresh backups it checked against are gone.
        assert [b["tags"] for b in backups() if b["hostname"] == "machine-drill"] == [["drill"]], backups()

    with subtest("a snapshot older than staleAfter fails the pass"):
        machine.succeed("date -s '+7 hours'")
        machine.fail("systemctl start machine-backup")
        machine.succeed("journalctl -u machine-backup | grep 'is more than 6 hours old'")
  '';
}
