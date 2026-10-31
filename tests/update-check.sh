# HOST-update's VM tests take a heavy slot (modules/updates/check.sh), on a
# machine that has them, and say how the test went either way.
#
#     bash tests/update-check.sh modules/updates/check.sh
set -euo pipefail

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# shellcheck disable=SC2034 # read by check.sh
work=/work SYSTEM=x86_64-linux
# shellcheck source=/dev/null
source "$1"

# Stands in for runuser: runs what it's given, with `nix` answering from $tmp/rc.
as_user() { "$@"; }
nix() { echo "nix $*" >>"$tmp/calls"; return "$(cat "$tmp/rc")"; }
slot() { [ "$1" = -- ] && shift; echo "slot taken" >>"$tmp/calls"; "$@"; }
fail() { echo "FAIL: $1"; cat "$tmp/calls"; exit 1; }
want='nix build --no-link -L git+file:///work?rev=abc#checks.x86_64-linux.agents'

# shellcheck disable=SC2034 # read by check.sh
HEAVY=slot
echo 0 >"$tmp/rc"; : >"$tmp/calls"
run_check agents abc "$tmp/log" || fail "a passing test in a slot should pass"
[ "$(cat "$tmp/calls")" = "slot taken
$want" ] || fail "the test should run inside the slot"
echo 1 >"$tmp/rc"
if run_check agents abc "$tmp/log"; then fail "a failing test in a slot should fail"; fi

# shellcheck disable=SC2034 # read by check.sh
HEAVY=
echo 0 >"$tmp/rc"; : >"$tmp/calls"
run_check agents abc "$tmp/log" || fail "a passing test without slots should pass"
[ "$(cat "$tmp/calls")" = "$want" ] || fail "with no slots the test should just run"
echo 1 >"$tmp/rc"
if run_check agents abc "$tmp/log"; then fail "a failing test without slots should fail"; fi
echo ok
