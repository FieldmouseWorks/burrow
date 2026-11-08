# lib/project.nix's package of a Rust workspace: every member's binaries.
{ self, pkgs, system, ... }:
let
  pkg = (self.lib.project { self = ../tests/house/rust; systems = [ system ]; rust = { }; }).packages.${system}.default;
in pkgs.runCommand "house-rust-package-check" { } ''
  # Its answer comes from modules named like tests that the library uses:
  # one declared plainly, one by a path attribute that a #[cfg(test)]
  # module's path attribute names too; and from one named by a raw
  # identifier that a #[cfg(test)] module reads too.
  [ "$(${pkg}/bin/house-fixture-member)" = 42 ]
  # What lib/project.nix's cargoTests must leave in: a program named tests,
  # and a member named tests that a glob finds (a module named tests that
  # isn't one, and a crate in tests/, the build itself needs).
  [ "$(${pkg}/bin/tests)" = tests ]
  [ "$(${pkg}/bin/main)" = main ]
  [ "$(${pkg}/bin/house-fixture-tests-crate)" = crate ]
  # And a package's tests whose role in its build can't be told: a build
  # script in tests/, a program that includes a file from it by a path it
  # makes, one whose module is there by a conditional path attribute (and
  # a test target declared at a module's file, which the build itself
  # needs).
  [ "$(${pkg}/bin/house-fixture-built)" = built ]
  [ "$(${pkg}/bin/house-fixture-included)" = 9 ]
  [ "$(${pkg}/bin/house-fixture-pathed)" = 5 ]
  # A program at Cargo's default path that a #[cfg(test)] module reads too
  # and that includes a test module's file, and a module inside an inline
  # one whose file a #[cfg(test)] module reads too.
  [ "$(${pkg}/bin/house-fixture-implicit)" = implicit ]
  [ "$(${pkg}/bin/house-fixture-nested)" = nested ]
  # A program in src/bin, where a #[cfg(test)] module's own modules would
  # be; modules found from a #[path]'s file and from a module named main;
  # and a module whose #[path] is a raw string.
  [ "$(${pkg}/bin/house-fixture-binmod)" = binmod ]
  [ "$(${pkg}/bin/house-fixture-contexts)" = contexts ]
  [ "$(${pkg}/bin/house-fixture-rawpath)" = rawpath ]
  # Declarations with comments among them (a #[cfg(test)] commented out,
  # one between a path attribute and its module), a cfg_attr path with a
  # space before its parenthesis, a module a macro declares by a path
  # attribute, and a module loaded from a crate inside the package that
  # Cargo doesn't load.
  [ "$(${pkg}/bin/house-fixture-commented)" = commented ]
  [ "$(${pkg}/bin/house-fixture-remarked)" = remarked ]
  [ "$(${pkg}/bin/house-fixture-spaced)" = spaced ]
  [ "$(${pkg}/bin/house-fixture-made)" = made ]
  [ "$(${pkg}/bin/house-fixture-across)" = across ]
  # And what the scan doesn't lex or parse: a nested block comment hiding
  # a quote, a shebang line ending in #[cfg(test)], #[r#path = ...], and a
  # cfg_attr path after an attribute with brackets inside it.
  [ "$(${pkg}/bin/house-fixture-deep)" = deep ]
  [ "$(${pkg}/bin/house-fixture-shebang)" = shebang ]
  [ "$(${pkg}/bin/house-fixture-rawattr)" = rawattr ]
  [ "$(${pkg}/bin/house-fixture-bracketed)" = bracketed ]
  # A package whose release build compiles a crate in its tests/ that
  # includes one of its test modules.
  [ "$(${pkg}/bin/house-fixture-outer)" = outer ]
  touch $out
''
