# Each flaky the machine runs asks GitHub through the first gh on its
# caller's PATH (the machine's, which knows an App's env), with no gh
# of its own ahead of it: the toolbox's, and the steward's, which
# burrow-upkeep runs as a library, reading the config the module
# writes (a field it doesn't know fails it).
{ self, pkgs, ... }:
let
  example = self.nixosConfigurations.example.config;
  steward = builtins.head (builtins.filter (p: (p.name or "") == "autoland-steward")
    example.environment.systemPackages);
  # GitHub's answer to gh api -i for an empty quarantine, and a note
  # that it was asked.
  gh = pkgs.writeShellScript "gh" ''
    echo "$*" >>"$CALLED"
    printf 'HTTP/2.0 200 OK\nContent-Type: application/json\r\n\r\n[]'
  '';
in pkgs.runCommand "flaky-gh-check" { nativeBuildInputs = [ pkgs.jq ]; } ''
  mkdir bin && ln -s ${gh} bin/gh
  config=$(sed -n 's|^export AUTOLAND_CONFIG=.*:-\(/nix/store/[^}]*\)}$|\1|p' ${steward}/bin/autoland-steward)
  [ -n "$config" ] || { echo "autoland-steward names no config"; exit 1; }
  upkeep=$(jq -r '.upkeep | join(" ")' "$config")
  [ "$(jq -r '.flaky' "$(jq -r '.upkeep[1]' "$config")")" != null ] || { echo "the steward's upkeep has no flaky tests"; exit 1; }
  for flaky in "${example.system.path}/bin/flaky quarantine sam/r" "$upkeep watch sam/r --now 1800000000"; do
    rm -f called
    said=$(env -i PATH=$PWD/bin CALLED=$PWD/called $flaky 2>&1) || { echo "$flaky: $said"; exit 1; }
    grep -q '^api .*repos/sam/r/issues?labels=flaky' called || { echo "$flaky asked GitHub through a gh of its own: $said"; exit 1; }
  done
  touch $out
''
