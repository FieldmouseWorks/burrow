# The review agent the steward runs, from npm: the codex CLI. package.json
# pins it and Renovate keeps it at its newest release; the lockfile's own
# hashes fetch it, so a bump needs nothing else.
{ pkgs }:
let
  inherit (pkgs) lib;
  lock = lib.importJSON ./package-lock.json;
  # npm's own test of a package's os or cpu list against this machine's:
  # none of its `!name`s, and one of its names if it has any.
  fits = value: list:
    let
      negated = map (lib.removePrefix "!") (lib.filter (lib.hasPrefix "!") list);
      named = lib.filter (n: !lib.hasPrefix "!" n) list;
    in
    !lib.elem value negated && (named == [ ] || lib.elem value named);
  node = pkgs.stdenv.hostPlatform.node;
  # codex's native binaries are optional packages, one for each platform,
  # each 130-170 MiB; npm installs only this machine's, so the others are
  # left out of the lock rather than fetched.
  here = module:
    !(module.optional or false) || (fits node.platform (module.os or [ ]) && fits node.arch (module.cpu or [ ]));
in
pkgs.buildNpmPackage {
  pname = "review-agents";
  version = (builtins.fromJSON (builtins.readFile ./package.json)).dependencies."@openai/codex";
  src = pkgs.lib.fileset.toSource {
    root = ./.;
    fileset = pkgs.lib.fileset.unions [ ./package.json ./package-lock.json ];
  };
  npmDeps = pkgs.importNpmLock {
    npmRoot = ./.;
    packageLock = lock // { packages = lib.filterAttrs (_: here) lock.packages; };
  };
  npmConfigHook = pkgs.importNpmLock.npmConfigHook;
  npmFlags = [ "--ignore-scripts" ];
  dontNpmBuild = true;
  installPhase = ''
    mkdir -p $out/lib $out/bin
    cp -r node_modules $out/lib/
    ln -s $out/lib/node_modules/@openai/codex-linux-x64/vendor/x86_64-unknown-linux-musl/bin/codex $out/bin/codex
  '';
  # It runs: an update that breaks it (a new native piece, say) doesn't build.
  doInstallCheck = true;
  installCheckPhase = ''
    HOME=$TMPDIR $out/bin/codex --version
  '';
  meta.mainProgram = "codex";
}
