# The host the VM tests that call engines' plugins share (../agents-plugins.nix,
# ../../lib/engine-conformance.nix): a proj job calls them, as a Conary
# runner would, so the host's services a grant reaches (proj's sockets, the
# probe, heavy, the unit watch, an MCP server) are the real ones. Its engine
# runs /tmp/calls, a shell script the test writes, with the job's sockets
# and a login environment that holds a token, and leaves its exit in
# /tmp/calls.exit. An engine's host, at 443, answers `engine-reached`.
{ self }:
{ pkgs, ... }:
let
  inherit (import ../proj/stand-ins.nix { inherit pkgs; }) gh;
  caller = pkgs.writeShellScriptBin "claude" ''
    [ "''${1:-}" = --version ] && { echo "caller fixture"; exit 0; }
    cat >/dev/null
    echo '{"type":"system","subtype":"init","session_id":"caller"}'
    export GH_TOKEN=caller-gh-token
    sh /tmp/calls >/tmp/calls.log 2>&1
    echo $? >/tmp/calls.exit
    proj done >/dev/null
    echo '{"type":"result","session_id":"caller","result":"called","is_error":false}'
  '';
  # An MCP server that says who ran it, with what.
  notes = pkgs.writeShellScript "notes-mcp" ''
    read -r line
    echo "$(id -un) $SETTING ''${GH_TOKEN-no-token} $line"
  '';
in
{
  imports = [ self.nixosModules.default ./host.nix ];
  burrow = {
    claude.enable = true;
    claude.package = caller;
    proj.enable = true;
    proj.gh = gh;
    probe.repo = "o/machine";
    cpu.enable = true;
    cpu.heavy.slots = 1;
    watchdog.units.enable = true;
    mcpServers.notes = {
      command = "${notes}";
      env.SETTING = "registry-setting";
      envVars = [ "GH_TOKEN" ];
    };
    agents.enable = true;
    agents.memory = 1536;
    agents.vcpus = 2;
  };

  programs.git.enable = true;
  environment.systemPackages = [
    self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-example-plugin
    pkgs.socat
  ];
  boot.kernelModules = [ "vhost_vsock" ];

  systemd.services.reach-tcp = {
    wantedBy = [ "multi-user.target" ];
    path = [ pkgs.socat pkgs.coreutils pkgs.bash ];
    serviceConfig.Restart = "on-failure";
    script = "exec socat TCP4-LISTEN:443,fork,reuseaddr SYSTEM:'echo engine-reached'";
  };
}
