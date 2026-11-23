# Sandboxes for real: boot two, work in them, break out of nothing; run an
# engine's plugin in one of its own, with its grants and nothing else; and
# hold each engine's plugin to its conformance check there
# (lib/engine-conformance.nix). Nested KVM: run it on a machine that has
# /dev/kvm. One check, since a machine proves it by this name (CI only
# evaluates it): an engine's plugin adds its conformance here.
{ self, pkgs, ... }:
let
  example = self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-example-plugin;
  # The example plugin as an engine that needs the network and no MCP server
  # or KVM, so the check sees both halves: what it declares works, and what
  # it doesn't is unavailable however much is approved.
  networked = pkgs.runCommand "example-plugin-networked" {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = "example-plugin-networked";
  } ''
    makeBinaryWrapper ${pkgs.lib.getExe' example "burrow-example-plugin"} $out/bin/example-plugin-networked \
      --set EXAMPLE_PLUGIN_DECLARES network
  '';
  inherit (import ../tests/agents/capabilities.nix) grantable;
  antigravityStandIn = pkgs.writeShellScriptBin "antigravity" ''exit 0'';
  antigravityConfig = pkgs.writeText "antigravity-conformance.json" (builtins.toJSON {
    binary = pkgs.lib.getExe antigravityStandIn;
    models = [
      {
        id = "test-model";
        efforts = [ "low" "medium" "high" "xhigh" "max" ];
      }
    ];
    default_model = "test-model";
    default_effort = "medium";
    network = true;
    mcp = false;
    devices = [ ];
    meters = [ "quota" ];
  });
  antigravityPlugin = pkgs.runCommand "antigravity-plugin-conformance" {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = "burrow-engine-antigravity";
  } ''
    makeBinaryWrapper ${pkgs.lib.getExe' self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-engine-antigravity "burrow-engine-antigravity"} \
      $out/bin/burrow-engine-antigravity \
      --set BURROW_ANTIGRAVITY_CONFIG ${antigravityConfig}
  '';
in
pkgs.linkFarm "agents" {
  sandboxes = import ../tests/agents.nix { inherit self pkgs; };
  plugins = import ../tests/agents-plugins.nix { inherit self pkgs; };
  conformance-example = self.lib.engineConformance {
    inherit pkgs;
    name = "example";
    plugin = networked;
    capabilities = pkgs.lib.subtractLists [ "kvm" "mcp" ] grantable;
  };
  conformance-antigravity = self.lib.engineConformance {
    inherit pkgs;
    name = "antigravity";
    plugin = antigravityPlugin;
    capabilities = pkgs.lib.subtractLists [ "kvm" "mcp" ] grantable;
    engine = {
      packages = [ antigravityStandIn ];
      credentialEnv = "BURROW_ANTIGRAVITY_CREDENTIAL";
    };
  };
}
