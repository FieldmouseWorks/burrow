# The example's cargo-follow reads the config its module writes (a field the
# binary doesn't know fails it), and says the manifests it keeps — including
# `first` for repos in burrow.autoland.pipelineRepos / leadRepos and none for
# non-pipeline repos or when autoland is off.
{ self, pkgs, ... }:
let
  followOf = machine: pkgs.lib.getExe (builtins.head (builtins.filter (p: (p.name or "") == "cargo-follow")
    machine.config.environment.systemPackages));
  example = followOf self.nixosConfigurations.example;
  pipeline = followOf (self.nixosConfigurations.example.extendModules {
    modules = [
      {
        burrow.autoland = {
          pipelineRepos = [ "Sam/Homelab" ];
          leadRepos = [ "sam/burrow" ];
          labels.first = "urgent";
        };
        burrow.updates.follow.manifests = [
          { repo = "sam/burrow"; path = "crates/Cargo.toml"; }
          { repo = "sam/other"; path = "Cargo.toml"; }
        ];
      }
    ];
  });
  noAutoland = followOf (self.nixosConfigurations.example.extendModules {
    modules = [
      {
        burrow.autoland = {
          enable = pkgs.lib.mkForce false;
          pipelineRepos = [ "sam/homelab" ];
        };
      }
    ];
  });
in pkgs.runCommand "cargo-follow-config-check" { } ''
  got=$(${example} config)
  [ "$got" = "sam/homelab:tools/Cargo.toml" ] || { echo "example got: $got"; exit 1; }

  got=$(${pipeline} config)
  expected=$(printf '%s\n' \
    "sam/homelab:tools/Cargo.toml [urgent]" \
    "sam/burrow:crates/Cargo.toml [urgent]" \
    "sam/other:Cargo.toml")
  [ "$got" = "$expected" ] || { echo "pipeline got:"; echo "$got"; echo "expected:"; echo "$expected"; exit 1; }

  got=$(${noAutoland} config)
  [ "$got" = "sam/homelab:tools/Cargo.toml" ] || { echo "noAutoland got: $got"; exit 1; }

  touch $out
''
