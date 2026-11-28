# A finished job's scratch goes once proj sets it aside, by the root-side
# unit that alone removes it (modules/proj/scratch.nix, crates/burrow-scratch):
# whatever is in it that the job's own user can't remove (root's files,
# another service user's behind a dir only they can open, its own build
# output left read-only) goes too, its worktree off the repo with the unit
# still watching; a mount inside one is never crossed, the entry set aside as
# stuck with one notice while the rest still go, however many sweeps run, and
# goes once the mount does; and one set aside as a link loses only the link.
#
#   nix build .#checks.x86_64-linux.proj-scratch -L
{ self, pkgs }:
import ./proj/test.nix { inherit self pkgs; } "proj-scratch" ''
  J = "/data/dev/scratch/jobs"
  # The stand-in notifier writes here as whoever runs it: proj's as the user,
  # the stuck notices' as root (the real one runs itself as the user), so
  # it's root's, as /tmp is, and anyone's to write.
  machine.succeed("rm -f /tmp/notices && install -m 666 /dev/null /tmp/notices")

  def aside(*names):
      # No set aside or stuck name left in the jobs dir, of these or any.
      machine.wait_until_succeeds(
          f"test -z \"$(find {J} -mindepth 1 -maxdepth 1 \\( -name '.gone-*' -o -name '.stuck-*' \\) {' '.join('! -name ' + repr(n) for n in names)})\"",
          timeout=60,
      )

  def watching():
      machine.succeed("systemctl is-active proj-scratch-sweep.path")
      assert machine.succeed("systemctl show -P Result proj-scratch-sweep.path").strip() == "success"

  def settled():
      # The sweep and the notices after it both done.
      machine.wait_until_succeeds(
          "! systemctl is-active proj-scratch-sweep.service proj-scratch-stuck.service | grep -Eqx 'active|activating|deactivating|reloading'",
          timeout=60,
      )

  def notices(word):
      return int(machine.succeed(f"grep -c '^{word} scratch-stuck-[.]stuck-m-' /tmp/notices || true").strip() or 0)

  with subtest("a job's scratch goes with what its own user can't remove, its worktree off the repo"):
      machine.succeed("su - dev -c 'git -C /data/dev/src/web init -q && git -C /data/dev/src/web -c user.name=t -c user.email=t@t commit -q --allow-empty -m one'")
      j = proj('run web "scratch ask"')
      machine.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -x done", timeout=120)
      s = f"{J}/{j}"
      machine.succeed(f"test -e {s}/web/built")
      # Root's (a chroot a job's tool made), another service user's behind
      # a dir only it opens, and the job's own build output left read-only.
      machine.succeed(f"mkdir -p {s}/chroot/nix/store/x && touch {s}/chroot/nix/store/x/f && chmod -R a-w {s}/chroot")
      machine.succeed(f"mkdir -p {s}/svc/inner && touch {s}/svc/inner/f && chown -R nobody:nogroup {s}/svc && chmod 700 {s}/svc {s}/svc/inner")
      machine.succeed(f"su - dev -c 'mkdir -p {s}/ro/d && touch {s}/ro/d/f && chmod -R a-w {s}/ro'")
      proj(f"stop {j}")
      machine.wait_until_succeeds(f"! test -e {s}", timeout=60)
      aside()
      out = machine.succeed("su - dev -c 'git -C /data/dev/src/web worktree list --porcelain'")
      assert out.count("worktree ") == 1, out
      watching()

  with subtest("a mount inside is never crossed: stuck with one notice, the rest still go"):
      machine.succeed("mkdir -p /srv/held && echo kept >/srv/held/f")
      # Made under names nobody watches, then set aside together.
      machine.succeed(f"mkdir -p {J}/m/mnt {J}/p/x && touch {J}/p/x/f && mount --bind /srv/held {J}/m/mnt")
      machine.succeed(f"mv {J}/m {J}/.gone-m && mv {J}/p {J}/.gone-p")
      machine.wait_until_succeeds(f"test -d {J}/.stuck-m && ! test -e {J}/.gone-m && ! test -e {J}/.gone-p", timeout=60)
      aside(".stuck-m")
      machine.succeed(f"mountpoint -q {J}/.stuck-m/mnt")
      assert machine.succeed(f"cat {J}/.stuck-m/mnt/f /srv/held/f") == "kept\nkept\n"
      settled()
      assert notices("ensure") == 1, machine.succeed("cat /tmp/notices")
      # A job's end after another's, more of them than a unit's default
      # start limit lets through: each still goes, the stuck one is tried
      # again each time and hears nothing more, and the unit keeps watching.
      for i in range(8):
          machine.succeed(f"mkdir -p {J}/q{i}/d && touch {J}/q{i}/d/f && mv {J}/q{i} {J}/.gone-q{i}")
          machine.wait_until_succeeds(f"! test -e {J}/.gone-q{i}", timeout=60)
      aside(".stuck-m")
      settled()
      watching()
      assert notices("ensure") == 1, machine.succeed("cat /tmp/notices")
      assert notices("close") == 0, machine.succeed("cat /tmp/notices")
      assert machine.succeed("cat /srv/held/f") == "kept\n"
      # Unmounted, the next sweep removes it, and its notice closes.
      machine.succeed(f"umount {J}/.stuck-m/mnt")
      machine.succeed(f"mkdir {J}/.gone-r")
      aside()
      settled()
      assert notices("close") == 1, machine.succeed("cat /tmp/notices")
      assert notices("ensure") == 1, machine.succeed("cat /tmp/notices")
      assert machine.succeed("cat /srv/held/f") == "kept\n"
      watching()

  with subtest("one set aside as a link loses only the link"):
      machine.succeed("mkdir -p /srv/target/d && echo kept >/srv/target/d/f")
      machine.succeed(f"mkdir -p {J}/t && ln -s /srv/target {J}/t/to-dir && ln -s /srv/target/d/f {J}/t/to-file")
      machine.succeed(f"ln -s /srv/target {J}/.gone-link && mv {J}/t {J}/.gone-t")
      aside()
      assert machine.succeed("cat /srv/target/d/f") == "kept\n"
      machine.succeed("test -d /srv/target/d")
      watching()
''
