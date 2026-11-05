# The example's rustc wrapper reads the config its module writes: a
# field the binary doesn't know is a line on stderr at every compile,
# which goes uncached. Here, with no cache, it compiles without one
# and says nothing.
{ self, pkgs, ... }:
pkgs.runCommand "rustc-wrapper-config-check" { } ''
  said=$(${self.nixosConfigurations.example.config.environment.etc."rust-cache/rustc-wrapper".source} ${pkgs.coreutils}/bin/echo compiled 2>&1)
  [ "$said" = compiled ] || { echo "the wrapper said: $said"; exit 1; }
  touch $out
''
