# The authorized authors are declared once, `burrow.authors`
# (modules/common.nix), and every door reads that option. The scan is
# crates/burrow-authors-once, whose tests hold its rules; this runs it on
# burrow's own modules, and on a copy with a door's old option put back,
# which it must refuse (it did on the tree before this check, where each
# door declared its own).
{ pkgs, crates, system, ... }:
let scan = pkgs.lib.getExe crates.packages.${system}.burrow-authors-once;
in pkgs.runCommand "authors-once-check" { } ''
  ${scan} ${../.} || { echo "burrow's own modules break the one-authors-list rule" >&2; exit 1; }

  mkdir -p bad
  cp -r ${../modules} bad/modules
  cp -r ${../lib} bad/lib
  chmod -R u+w bad
  cat >> bad/modules/probe.nix <<'NIX'
  options.burrow.probe.authors = lib.mkOption { type = lib.types.listOf (import ../lib/github-author.nix { inherit lib; }); default = [ ]; };
NIX
  if ${scan} "$PWD/bad" 2>/dev/null; then
    echo "the scan accepted a second authors option in probe.nix" >&2; exit 1
  fi
  cp ${../modules/probe.nix} bad/modules/probe.nix; chmod u+w bad/modules/probe.nix
  sed -i 's/inherit (config.burrow) authors;/authors = [ { login = "other"; id = 7; } ];/' bad/modules/probe.nix
  if ${scan} "$PWD/bad" 2>/dev/null; then
    echo "the scan accepted a door with its own list of authors" >&2; exit 1
  fi
  touch $out
''
