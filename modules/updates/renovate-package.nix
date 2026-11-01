# Renovate, with its cargo manager's "Cargo.lock is unchanged" working. It read
# the lock file before and after `cargo update` as Buffers and compared them
# with ===, which is never true, so a refresh with nothing to do still handed
# back the lock file. Committing that came to nothing: "Detected empty commit"
# on the dashboard's Repository Problems, every run, for every repo whose
# crates were current. Upstream has since read both as text; --replace-warn so
# that a nixpkgs which has that builds anyway, and then this file can go. The
# renovate-cargo-unchanged check holds it to the behaviour either way.
pkgs:
pkgs.renovate.overrideAttrs (old: {
  postPatch = (old.postPatch or "") + ''
    substituteInPlace lib/modules/manager/cargo/artifacts.ts \
      --replace-warn '? await readLocalFile(lockFileName)' "? await readLocalFile(lockFileName, 'utf8')" \
      --replace-warn 'const newCargoLockContent = await readLocalFile(lockFileName);' "const newCargoLockContent = await readLocalFile(lockFileName, 'utf8');"
  '';
})
