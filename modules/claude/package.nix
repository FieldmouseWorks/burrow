# Claude Code moves fast, so it tracks nixpkgs-unstable on its own. One
# definition for the host and the agent sandboxes, so both run the same build.
# With a manifest (burrow.claude.manifest), nixpkgs' recipe builds that release
# instead, so neither waits for nixpkgs to bump it.
#
# nixpkgs' wrapper exports LD_LIBRARY_PATH with unstable's alsa-lib, for the
# libasound only local voice capture loads. Everything Claude runs inherits it,
# and a program built against an older glibc that loads libasound (QEMU) dies
# on a GLIBC version it doesn't have. A burrow machine is driven remotely, with
# no microphone to capture from, so the wrapper loses the variable instead of
# scoping it (patchelf breaks the bun binary, and running it through ld.so
# breaks Claude's self-spawns). checks/claude-library-path.nix holds it.
{ inputs, system, manifest ? null }:
let
  claude-code = (import inputs.nixpkgs-unstable {
    inherit system;
    config.allowUnfree = true;
  }).claude-code;
  recipe =
    if manifest == null then claude-code
    else claude-code.override { manifest = builtins.fromJSON (builtins.readFile manifest); };
in
recipe.overrideAttrs (old: {
  postInstall = (old.postInstall or "") + ''
    sed -i '/LD_LIBRARY_PATH/d' $out/bin/claude
  '';
})
