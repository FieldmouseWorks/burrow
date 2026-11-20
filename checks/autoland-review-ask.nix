# The owner's door's command (autoland/review-ask.nix), as the steward and
# conaryd's flow run it with REPO PR HEAD: it starts the door as a unit of
# the caller's own user manager (which passes it none of the caller's
# environment), waited for, one unit a head, with only the PR it's for, and
# its exit and what it prints are the door's. Here systemd-run is a
# stand-in that says what it was asked and exits as the door would.
{ self, pkgs, nixpkgs, ... }:
let
  lib = nixpkgs.lib;
  machine = (self.nixosConfigurations.example.extendModules {
    modules = [{ burrow.autoland.askAsOwner.repos = [ "sam/r" ]; }];
  }).config;
  door = machine.burrow.autoland.askAsOwner.door;
  launch = builtins.head door.command;
  systemdRun = "${machine.systemd.package}/bin/systemd-run";
  head = "0123456789abcdef0123456789abcdef01234567";
in pkgs.runCommand "autoland-review-ask-check" { } ''
  fail() { echo "$*"; exit 1; }
  [ ${toString (builtins.length door.command)} = 1 ] || fail "the door's command isn't the one launcher"
  grep -q ${systemdRun} ${launch} || fail "the launcher doesn't start the door with the machine's systemd-run"
  cat > stub <<EOF
  #!${pkgs.runtimeShell}
  printf '%s\n' "\$@" > $PWD/args
  printf '%s\n' "\$XDG_RUNTIME_DIR" > $PWD/runtime
  echo '{"outcome": "posted", "at": 7}'
  exit \''${STUB_EXIT:-0}
  EOF
  chmod +x stub
  sed "s|${systemdRun}|$PWD/stub|" ${launch} > launch
  chmod +x launch
  grep -q "$PWD/stub" launch || fail "the stand-in didn't replace systemd-run"

  said=$(env -u XDG_RUNTIME_DIR ./launch sam/r 5 ${head}) || fail "the launcher failed where the door didn't"
  [ "$said" = '{"outcome": "posted", "at": 7}' ] || fail "the launcher doesn't print what the door said: $said"
  unit=autoland-ask-$(printf '%s' 'sam/r#5@${head}' | sha256sum | cut -c1-16)
  door=$(sed -n 's|.* -- \(/nix/store/[^ ]*-burrow-review-ask\) "\$@"$|\1|p' ${launch})
  [ -x "$door" ] || fail "the launcher doesn't start the door: $(cat ${launch})"
  printf '%s\n' --user --wait --pipe --quiet --collect "--unit=$unit" -- "$door" sam/r 5 ${head} > want
  cmp want args || fail "the launcher asks systemd-run for something else: $(cat args)"
  [ "$(cat runtime)" = "/run/user/$(id -u)" ] || fail "the launcher doesn't find the caller's user manager: $(cat runtime)"

  # Its exit is the door's: conaryd records a head as asked only on 0.
  STUB_EXIT=1 ./launch sam/r 5 ${head} >/dev/null && fail "a door that failed reads as asked"
  XDG_RUNTIME_DIR=/run/user/elsewhere ./launch sam/r 5 ${head} >/dev/null
  [ "$(cat runtime)" = /run/user/elsewhere ] || fail "the launcher overrides the caller's own runtime directory"
  touch $out
''
