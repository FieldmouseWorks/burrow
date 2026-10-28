# The review agent the steward runs, from npm: the codex CLI. package.json
# pins it and Renovate keeps it at its newest release; the lockfile's own
# hashes fetch it, so a bump needs nothing else.
{ pkgs }:
pkgs.buildNpmPackage {
  pname = "review-agents";
  version = (builtins.fromJSON (builtins.readFile ./package.json)).dependencies."@openai/codex";
  src = pkgs.lib.fileset.toSource {
    root = ./.;
    fileset = pkgs.lib.fileset.unions [ ./package.json ./package-lock.json ];
  };
  npmDeps = pkgs.importNpmLock { npmRoot = ./.; };
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
