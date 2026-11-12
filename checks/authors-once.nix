# The authorized authors are declared once, `burrow.authors` (modules/common.nix),
# and every door reads that option: no module declares a second authors-like
# option (any option of lib/github-author.nix's type, or one named `authors`),
# and no door takes `authors` from anywhere else. The autoland modules' own
# `authors` are PR-author logins for the steward, not the doors' accounts, so
# they are left out. The scan runs on burrow's modules, and on a copy with a
# door's old option put back, which it must refuse (it did on the tree before
# this check, where each door declared its own).
{ pkgs, ... }:
pkgs.runCommand "authors-once-check" { nativeBuildInputs = [ pkgs.gnugrep pkgs.findutils ]; } ''
  scan() {
    dir=$1
    bad=0
    files=$(cd "$dir" && find modules lib -name '*.nix' ! -path modules/common.nix \
      ! -path modules/autoland.nix ! -path modules/autoland/shadow.nix \
      ! -path modules/watchdog/degradations.nix ! -path lib/github-author.nix | sort)
    for f in $files; do
      if grep -nE '^\s*authors\s*=\s*(lib\.)?(mkOption|types\.)|github-author\.nix' "$dir/$f"; then
        echo "$f declares an authors option of its own: use burrow.authors" >&2; bad=1
      fi
      if grep -nE 'inherit \((cfg|settings)[^)]*\)[^;]*\bauthors\b|^\s*authors\s*=\s*(cfg|settings)\.' "$dir/$f"; then
        echo "$f takes authors from somewhere but config.burrow.authors" >&2; bad=1
      fi
    done
    return $bad
  }

  scan ${../.} || { echo "burrow's own modules break the one-authors-list rule" >&2; exit 1; }

  # The tree as it was: probe.nix declaring its own list again.
  mkdir -p bad
  cp -r ${../modules} bad/modules
  cp -r ${../lib} bad/lib
  chmod -R u+w bad
  cat >> bad/modules/probe.nix <<'EOF'
  options.burrow.probe.authors = lib.mkOption { type = lib.types.listOf (import ../lib/github-author.nix { inherit lib; }); default = [ ]; };
EOF
  if scan "$PWD/bad" 2>/dev/null; then
    echo "the scan accepted a second authors option in probe.nix" >&2; exit 1
  fi
  cp ${../modules/probe.nix} bad/modules/probe.nix; chmod u+w bad/modules/probe.nix
  sed -i 's/inherit (config.burrow) authors;/inherit (cfg) authors;/' bad/modules/probe.nix
  if scan "$PWD/bad" 2>/dev/null; then
    echo "the scan accepted a door taking authors from its own cfg" >&2; exit 1
  fi
  touch $out
''
