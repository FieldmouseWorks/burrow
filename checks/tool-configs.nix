# Each tool burrow builds from a crate with its config built in, as
# the example machine builds it (the crates' own tests are the
# rest): its config is one its binary takes, every setting in it one
# it knows, and a word it doesn't know exits 2 with its usage before
# it runs anything.
{ self, pkgs, ... }:
let
  inherit (self.nixosConfigurations.example.config) burrow systemd;
  # The tool `exe`, whose usage is `line`.
  usage = exe: line: ''
    rc=0
    ${exe} --help 2>err || rc=$?
    [ "$rc" = 2 ] || { echo "${exe}: a usage exited $rc: $(cat err)"; exit 1; }
    grep -qxF ${pkgs.lib.escapeShellArg "usage: ${line}"} err || { echo "${exe} said: $(cat err)"; exit 1; }
  '';
  tool = name: pkgs.lib.getExe burrow.tools.${name};
  p = burrow.prefix;
in pkgs.runCommand "tool-configs-check" { } (
  usage (tool "update") "${p}-update [--force | status]"
  + usage (tool "switch") "${p}-switch [FLAKE_DIR | --toplevel PATH | --status | --abort] [--allow-downgrade]"
  + usage (tool "confirm") "${p}-confirm"
  + usage (tool "boot") "${p}-boot default SYSTEM | trial [--force] | check | status"
  + usage systemd.services.firmware-check.serviceConfig.ExecStart "firmware-check [--review]"
  + usage systemd.services.renovate.serviceConfig.ExecStart "renovate-run [OWNER/REPO]"
  + usage systemd.services.renovate-sweep.serviceConfig.ExecStart "renovate-sweep [OWNER/REPO]"
  + "touch $out\n")
