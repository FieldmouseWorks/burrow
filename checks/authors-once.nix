# Who may hand the machine issues is declared once, as `burrow.authors`, and
# every door reads that. This fails on a second option whose name says
# authors anywhere under `burrow` (`autoland.authors` and the degradations
# watch's are a different fact: whose pull requests count, by login), and on a
# module that takes its authors from any config but `config.burrow.authors`.
# The text scan skips autoland and the degradations watch, which own the PR-author lists.
{ self, pkgs, ... }:
let
  inherit (pkgs) lib;
  options = self.nixosConfigurations.example.options.burrow;
  # Option paths under burrow, without going into submodule types.
  paths = prefix: tree:
    lib.concatLists (lib.mapAttrsToList (name: v:
      let
        here = prefix ++ [ name ];
        walk = builtins.tryEval (
          if !(builtins.isAttrs v) then [ ]
          else if v ? _type then (if v._type == "option" then [ here ] else [ ])
          else paths here v);
      in if walk.success then walk.value else [ ]) tree);
  authorsLike = lib.filter (p: lib.last p == "authors") (paths [ ] options);
  names = map (lib.concatStringsSep ".") authorsLike;
  allowed = [ "authors" "autoland.authors" "watchdog.degradations.authors" ];
  extra = lib.subtractLists allowed names;
  modules = lib.fileset.toSource {
    root = ../.;
    fileset = ../modules;
  };
in
assert lib.assertMsg (extra == [ ])
  "burrow.authors is the one list of authorized authors; second options: ${toString extra}";
assert lib.assertMsg (lib.elem "authors" names) "burrow.authors is not declared";
pkgs.runCommand "authors-once-check" { nativeBuildInputs = [ pkgs.gnugrep ]; } ''
  cd ${modules}
  # A door's authors come from config.burrow.authors alone.
  bad=$(grep -rnE '(cfg[A-Za-z.]*|config\.burrow\.[A-Za-z.]+)\.authors|inherit \((cfg|config\.burrow\.[A-Za-z.]+)[^)]*\)[^;]*\bauthors\b' modules --include='*.nix' --exclude-dir=autoland --exclude=autoland.nix --exclude=degradations.nix || true)
  if [ -n "$bad" ]; then
    echo "a door takes authors from somewhere but config.burrow.authors:" >&2
    echo "$bad" >&2
    exit 1
  fi
  touch $out
''
