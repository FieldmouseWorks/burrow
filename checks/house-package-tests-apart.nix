# lib/project.nix's package leaves the workspace's tests out of its source
# (its cargoTests): the same workspace without them makes the same package,
# but not the same rust-test, which runs them. So a PR that changes only
# tests rebuilds the checks that run tests, not every check built from the
# programs, and the negative run (flake-check.yml) builds just those.
# tests/house-members.nix shows the same of rust.members' packages.
{ self, pkgs, system, nixpkgs, ... }:
let
  inherit (nixpkgs) lib;
  fixture = "tests/house/rust";
  # The repo as a store path, as a flake's self is, with only the fixture
  # and the toolchain file its own links to, less what `keep` leaves out.
  project = keep: self.lib.project {
    self = builtins.path {
      path = ./..;
      name = "source";
      filter = path: type:
        let rel = lib.removePrefix "${toString ./..}/" (toString path); in
        keep rel type && lib.any (p: rel == p || lib.hasPrefix "${p}/" rel || lib.hasPrefix "${rel}/" p)
          [ fixture "crates/rust-toolchain.toml" ];
    };
    systems = [ system ];
    rust.dir = fixture;
  };
  whole = project (_: _: true);
  # Without the member's unit tests (src/tests.rs, a module's tests by a
  # path attribute, src/halve_tests.rs, and a module of tests by another
  # name, in src/sorting/) and its integration test (tests/).
  tests = map (t: "${fixture}/member/${t}") [ "src/tests.rs" "src/halve_tests.rs" "src/sorting/sorting_tests.rs" "tests" ];
  untested = project (rel: _: !(builtins.elem rel tests));
in
assert lib.assertMsg (lib.all (t: builtins.pathExists ../${t}) tests)
  "${fixture}'s member has lost the tests this check takes away";
assert lib.assertMsg (whole.packages.${system}.default.drvPath == untested.packages.${system}.default.drvPath)
  "${fixture}'s package changes with its tests: every check built from it would rebuild when only a test changes";
assert lib.assertMsg (whole.checks.${system}.rust-test.drvPath != untested.checks.${system}.rust-test.drvPath)
  "${fixture}'s rust-test doesn't change with the tests it runs";
pkgs.runCommand "house-package-tests-apart-check" { } "touch $out"
