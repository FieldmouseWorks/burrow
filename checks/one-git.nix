# One git for the machine: `programs.git.package` (modules/common.nix) is
# the git the machine and burrow's units run, so a patch to it reaches
# every one of them. Two halves:
#
# - No file under modules/ or lib/ names nixpkgs' own git (`pkgs.git`),
#   but for the one place that defines the package. Each file is read
#   through Nix's own parser, so a comment, a string or `pkgs.git-lfs`
#   isn't one.
# - A git a unit gets by name (`with pkgs; [ git ]`, which no scan of the
#   text can tell from the package) is caught on the example machine, with
#   the fresh runners on too: every git on a unit's path, or in the
#   system's packages, is the package. That covers what that machine
#   enables.
{ self, pkgs, ... }:
let
  inherit (pkgs) lib;
  machine = (self.nixosConfigurations.example.extendModules {
    modules = [{ burrow.dispatch.fresh = { enable = true; owner = "sam"; repos = [ "sam/homelab" ]; }; }];
  }).config;
  package = machine.programs.git.package;
  isGit = p: lib.isDerivation p && (p.meta.mainProgram or null) == "git";
  # Each unit, and the system's packages, with the gits that aren't the package.
  units = kind: lib.mapAttrsToList (n: s: { name = "${kind} ${n}"; path = s.path or [ ]; });
  places = units "unit" machine.systemd.services ++ units "user unit" machine.systemd.user.services
    ++ [{ name = "environment.systemPackages"; path = machine.environment.systemPackages; }];
  strays = lib.concatMap (u: map (p: "${u.name}: ${p.name}") (lib.filter (p: isGit p && p.outPath != package.outPath) u.path)) places;
  sources = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions (map (lib.fileset.fileFilter (f: f.hasExt "nix")) [ ../modules ../lib ]);
  };
  # What the scan must tell apart, so a parser whose output changes can't
  # leave it blind: one that names `pkgs.git` four ways, one that doesn't.
  names = pkgs.writeText "names.nix" ''
    { pkgs, ... }: { a = "''${pkgs.git}/bin/git"; b = with pkgs; pkgs . git; c = pkgs."git"; d = pkgs.git.overrideAttrs (o: o); }
  '';
  clean = pkgs.writeText "clean.nix" ''
    # pkgs.git, in a comment
    { pkgs, config, ... }: { a = pkgs.git-lfs; b = "pkgs.git, in a string"; c = config.programs.git.package; d = pkgs.gitty; }
  '';
in
pkgs.runCommand "one-git-check" { nativeBuildInputs = [ pkgs.nix ]; } ''
  export NIX_STATE_DIR=$TMPDIR/state NIX_LOG_DIR=$TMPDIR/log HOME=$TMPDIR
  # How often FILE, parsed, selects `git` from `pkgs`: the parser prints
  # each select as `(pkgs).git`, whatever its spelling in the file.
  count() {
    nix-instantiate --parse "$1" >$TMPDIR/parsed || { echo "$1 doesn't parse" >&2; exit 1; }
    { grep -oP "\(pkgs\)\.git(?![\w'-])" $TMPDIR/parsed || true; } | wc -l
  }
  [ "$(count ${names})" = 4 ] || { echo "the scan misses a pkgs.git: $(cat $TMPDIR/parsed)"; exit 1; }
  [ "$(count ${clean})" = 0 ] || { echo "the scan takes what isn't pkgs.git for it: $(cat $TMPDIR/parsed)"; exit 1; }

  bad=0
  cd ${sources}
  for f in $(find modules lib -name '*.nix' | sort); do
    n=$(count "$f")
    # The one place that may: where the package is made from it, once.
    want=0; [ "$f" != modules/common.nix ] || want=1
    [ "$n" = "$want" ] || { echo "$f names pkgs.git $n times, not $want: take config.programs.git.package"; bad=1; }
  done
  ${lib.concatMapStrings (s: "echo ${lib.escapeShellArg "a git that isn't programs.git.package: ${s}"}; bad=1\n") strays}
  [ "$bad" = 0 ] || exit 1
  touch $out
''
