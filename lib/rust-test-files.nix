# Which test files a workspace's `cargo test --workspace` runs (lib/project.nix's
# rust-test), as the git glob pathspecs from the repo's root a check's
# testFiles holds. The negative run (flake-check.yml) evaluates them on the
# base, so they're the base's packages: a test in a crate the base doesn't
# have, which no check there runs, is unrun rather than passing.
#
# - root: the workspace's directory, as a path;
# - prefix: that directory from the repo's root ("" for the root itself);
# - packages: the directories of the packages it tests, from root ("" for a
#   root package);
# - include: the paths from the repo's root its tests read beside it.
#
# A package is all its own (`a/**`, a first tests/ directory included). Any
# other directory there is the tests' fixtures (`fixtures/**`), but one
# that holds a package has only its own files (`crates/*`) and its
# packages, so a new crate beside them isn't run. Neither is anything else
# new there: the base can't tell a new directory of fixtures from a crate.
# A root package, or one holding others, has Cargo's own target
# directories too.
# Only builtins: the negative run's tests (crates/burrow-workflows) import it.
{ root, prefix, packages, include }:
let
  inherit (builtins) any attrNames concatMap elem readDir stringLength substring;
  hasPrefix = p: s: substring 0 (stringLength p) s == p;
  # A name as a glob takes it literally.
  escape = builtins.replaceStrings [ "\\" "*" "?" "[" ] [ "\\\\" "\\*" "\\?" "\\[" ];
  join = a: b: if a == "" then b else if b == "" then a else "${a}/${b}";
  at = rel: glob: join (escape (join prefix rel)) glob;
  holds = rel: rel == "" || any (p: hasPrefix "${rel}/" p) packages;
  # Cargo's own target directories, a package's whatever they hold.
  targets = rel: if elem rel packages then [ "src" "tests" "benches" "examples" ] else [ ];
  walk = rel:
    let
      entries = readDir (if rel == "" then root else root + "/${rel}");
      own = targets rel;
    in
    [ (at rel "*") ] ++ map (d: at (join rel d) "**") own
    ++ concatMap (name:
      let sub = join rel name; in
      if entries.${name} != "directory" || elem name own then [ ]
      else if holds sub then walk sub
      else [ (at sub "**") ]) (attrNames entries);
in
walk "" ++ concatMap (p: [ (escape p) "${escape p}/**" ]) include
