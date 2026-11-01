# lib/project.nix's rust.members, on tests/house/rust-members: each program
# a package of its own that runs, packages.default with both, and each built
# from only what it reaches, so a change to another member leaves it as it
# was.
{ self, pkgs }:
let
  inherit (pkgs) lib;
  system = pkgs.stdenv.hostPlatform.system;
  fixture = "tests/house/rust-members";
  # The repo as a store path, as a flake's self is, with only the fixture
  # and the toolchain file its own links to, less what `keep` leaves out.
  project = keep: (self.lib.project {
    self = builtins.path {
      path = ./..;
      name = "source";
      filter = path: _:
        let rel = lib.removePrefix "${toString ./..}/" (toString path); in
        keep rel && lib.any (p: rel == p || lib.hasPrefix "${p}/" rel || lib.hasPrefix "${rel}/" p)
          [ fixture "crates/rust-toolchain.toml" ];
    };
    systems = [ system ];
    rust = { dir = fixture; members = true; };
  }).packages.${system};
  all = project (_: true);
  hello = all.members-hello;
  # Without a file of the other program's.
  less = project (rel: rel != "${fixture}/other/src/seven.rs");
in
{
  house-members-package = pkgs.runCommand "house-members-package-check" { } ''
    [ "$(${hello}/bin/members-hello)" = 42 ] || { echo "members-hello didn't say 42"; exit 1; }
    [ "$(${all.members-other}/bin/members-other)" = 7 ] || { echo "members-other didn't say 7"; exit 1; }
    for p in members-hello members-other; do
      [ -x ${all.default}/bin/$p ] || { echo "packages.default has no $p"; exit 1; }
    done
    ${lib.optionalString (all ? members-shared) "echo 'the library members-shared is a package'; exit 1"}
    # members-hello is built from it and the library it reaches, and its
    # manifest and lockfile name only those.
    cd ${hello.src}
    got=$(find . ! -type d | sort)
    want=$(printf '%s\n' ./hello/Cargo.toml ./hello/src/main.rs ./shared/Cargo.toml ./shared/src/lib.rs)
    [ "$got" = "$want" ] || { printf "members-hello's source has:\n%s\n" "$got"; exit 1; }
    got=$(grep '^name = ' ${hello.cargoLock} | sort)
    want=$(printf '%s\n' 'name = "members-hello"' 'name = "members-shared"')
    [ "$got" = "$want" ] || { printf "members-hello's lockfile has:\n%s\n" "$got"; exit 1; }
    ! grep -q -e other -e itoa ${hello.cargoToml} || { echo "members-hello's manifest names members-other's member or dependency:"; cat ${hello.cargoToml}; exit 1; }
    touch $out
  '';
  house-members-apart = pkgs.runCommand "house-members-apart-check" { } (
    if hello.drvPath == less.members-hello.drvPath then "touch $out"
    else "echo '${fixture}/other/src/seven.rs, which members-hello never reaches, changes its package'; exit 1"
  );
}
