# Renovate, with two fixes of ours.
#
# Its cargo manager's "Cargo.lock is unchanged" works. It read the lock file
# before and after `cargo update` as Buffers and compared them with ===, which
# is never true, so a refresh with nothing to do still handed back the lock
# file. Committing that came to nothing: "Detected empty commit" on the
# dashboard's Repository Problems, every run, for every repo whose crates were
# current. Upstream has since read both as text; --replace-warn so that a
# nixpkgs which has that builds anyway, and then that part can go. The
# renovate-cargo-unchanged check holds it to the behaviour either way.
#
# Its dashboard leaves out an update that had nothing to commit and has no
# branch (patches/renovate-dashboard-no-work.patch). It listed one under Other
# Branches, pending, every run of its schedule: a lock file refresh that keeps
# changing nothing sat there for good, and the steward's dashboard watch took
# it for stuck. The renovate-dashboard-no-work check holds it to that.
pkgs:
pkgs.renovate.overrideAttrs (old: {
  patches = (old.patches or [ ]) ++ [ ../../patches/renovate-dashboard-no-work.patch ];
  postPatch = (old.postPatch or "") + ''
    substituteInPlace lib/modules/manager/cargo/artifacts.ts \
      --replace-warn '? await readLocalFile(lockFileName)' "? await readLocalFile(lockFileName, 'utf8')" \
      --replace-warn 'const newCargoLockContent = await readLocalFile(lockFileName);' "const newCargoLockContent = await readLocalFile(lockFileName, 'utf8');"
  '';
})
