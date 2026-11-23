# The example's cargo-follow reads the config its module writes (a field the
# binary doesn't know fails it), and says the manifests it keeps.
{ self, pkgs, ... }:
let
  follow = pkgs.lib.getExe (builtins.head (builtins.filter (p: (p.name or "") == "cargo-follow")
    self.nixosConfigurations.example.config.environment.systemPackages));
in pkgs.runCommand "cargo-follow-config-check" { } ''
  got=$(${follow} config)
  [ "$got" = "sam/homelab:tools/Cargo.toml" ] || { echo "got: $got"; exit 1; }
  touch $out
''
