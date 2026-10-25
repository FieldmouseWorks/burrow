# The review agents the steward runs, from npm: the codex CLI and DeepSeek
# Harness (dsh), with the official Node build dsh needs. package.json pins
# each one and Renovate keeps them at their newest release; the lockfile's
# own hashes fetch them, so a bump needs nothing else.
{ pkgs }:
pkgs.buildNpmPackage {
  pname = "review-agents";
  version = (builtins.fromJSON (builtins.readFile ./package.json)).dependencies."@deepseek-ai/dsh";
  src = pkgs.lib.fileset.toSource {
    root = ./.;
    fileset = pkgs.lib.fileset.unions [ ./package.json ./package-lock.json ];
  };
  npmDeps = pkgs.importNpmLock { npmRoot = ./.; };
  npmConfigHook = pkgs.importNpmLock.npmConfigHook;
  npmFlags = [ "--ignore-scripts" ];
  dontNpmBuild = true;
  nativeBuildInputs = [ pkgs.autoPatchelfHook pkgs.makeWrapper ];
  buildInputs = [ pkgs.stdenv.cc.cc.lib ];
  # dsh reads node's own internals out of its binary, so it needs the
  # official Node build, not nixpkgs'. npm installs every platform's
  # prebuilds: the musl ones go unused.
  autoPatchelfIgnoreMissingDeps = [ "libc.musl-x86_64.so.1" ];
  installPhase = ''
    mkdir -p $out/lib $out/bin
    cp -r node_modules $out/lib/
    m=$out/lib/node_modules
    makeWrapper $m/node-linux-x64/bin/node $out/bin/dsh --add-flags $m/@deepseek-ai/dsh/lib/bin.js
    ln -s $m/@openai/codex-linux-x64/vendor/x86_64-unknown-linux-musl/bin/codex $out/bin/codex
  '';
  # Each one runs: an update that breaks either (a new native piece left
  # unpatched, say) doesn't build.
  doInstallCheck = true;
  installCheckPhase = ''
    HOME=$TMPDIR $out/bin/dsh --version
    HOME=$TMPDIR $out/bin/codex --version
  '';
  meta.mainProgram = "dsh";
}
