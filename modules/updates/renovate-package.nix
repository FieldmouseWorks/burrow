# Renovate, with three fixes of ours.
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
# Its dashboard leaves out an update that had nothing to commit, has no
# branch and no artifact error (patches/renovate-dashboard-no-work.patch); one
# whose artifacts failed stays, to be retried. It listed one under Other
# Branches, pending, every run of its schedule: a lock file refresh that keeps
# changing nothing sat there for good, and the steward's dashboard watch took
# it for stuck. The renovate-dashboard-no-work check holds it to that.
#
# A crate several workspace members use bumps without an artifact error
# (patches/renovate-cargo-build-metadata.patch). Its cargo manager runs
# `cargo update --package NAME@OLD --precise NEW` once per member against the
# one lock file, so every member after the first finds OLD gone; it then drops
# a dependency the lock file already has at newVersion, but compared strings,
# and a lock version with build metadata (toml's `1.2.2+spec-1.1.0`) never
# matched newVersion (`1.2.2`). The renovate-cargo-build-metadata check holds
# it to that.
#
# With BURROW_RENOVATE_NO_MERGE in its environment it merges nothing: no PR,
# no branch onto its base, no platform automerge
# (patches/renovate-no-merge.patch). renovate-run sets it where the steward
# doesn't hold landing, so only conaryd moves that trunk. The
# renovate-no-merge check holds it to that.
pkgs:
pkgs.renovate.overrideAttrs (old: {
  patches = (old.patches or [ ]) ++ [
    ../../patches/renovate-dashboard-no-work.patch
    ../../patches/renovate-no-merge.patch
    ../../patches/renovate-cargo-build-metadata.patch
  ];
  postPatch = (old.postPatch or "") + ''
    substituteInPlace lib/modules/manager/cargo/artifacts.ts \
      --replace-warn '? await readLocalFile(lockFileName)' "? await readLocalFile(lockFileName, 'utf8')" \
      --replace-warn 'const newCargoLockContent = await readLocalFile(lockFileName);' "const newCargoLockContent = await readLocalFile(lockFileName, 'utf8');"
  '';
})
