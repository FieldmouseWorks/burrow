# burrow's Markdown names a fact only where it's owned (crates/burrow-guidance):
# an option given with a value, or the file a live one is kept in, anywhere
# but the generated settings reference fails, and so does an AGENTS.md whose
# house blocks aren't house/'s. Only the Markdown is its input,
# so a code change doesn't run it again.
{ self, pkgs, system, ... }:
let
  markdown = pkgs.lib.fileset.toSource {
    root = ../.;
    fileset = pkgs.lib.fileset.fileFilter (file: file.hasExt "md") ../.;
  };
in pkgs.runCommand "guidance-check" { } ''
  ${self.packages.${system}.burrow-guidance}/bin/burrow-guidance ${markdown}
  touch $out
''
