# What HOST runs against what HEAD builds (modules/updates/running.sh), on its
# own: real git, and a stand-in for nix whose system is made of flake.lock and
# system.nix and is stamped with the commit it's built from, as a real one is
# (the README, say, goes into neither).
#
#     bash tests/update-running.sh modules/updates/running.sh
set -euo pipefail

RUNNING=$(realpath "$1")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export HOME=$tmp GIT_CONFIG_NOSYSTEM=1
git config --global user.name test
git config --global user.email test@example.org
git config --global init.defaultBranch main
git config --global advice.detachedHead false

# nix build: HEAD's system as update.sh's build asks for it (WORK#...toplevel,
# stamped with HEAD), or a commit's as same_as_running does (an --expr with
# git+file://WORK?rev=COMMIT, stamped with what it forces). Every call goes
# to $tmp/nix.log. With $tmp/nix-down there, it fails as a failed build does.
mkdir "$tmp/bin"
cat >"$tmp/bin/nix" <<EOF
#!$(command -v bash)
set -euo pipefail
printf '%s\n' "\$*" >>"$tmp/nix.log"
[ ! -e "$tmp/nix-down" ] || { echo "error: build failed" >&2; exit 1; }
expr= ref=
for ((i = 1; i <= \$#; i++)); do
  case "\${!i}" in
    --expr) j=\$((i + 1)); expr=\${!j} ;;
    *'#nixosConfigurations.'*) ref=\${!i} ;;
  esac
done
if [ -n "\$expr" ]; then
  dir=\$(sed -n 's|.*git+file://\([^?]*\)?rev=.*|\1|p' <<<"\$expr")
  commit=\$(sed -n 's|.*?rev=\([0-9a-f]*\).*|\1|p' <<<"\$expr")
  stamp=\$(sed -n 's|.*mkForce \([^;]*\);.*|\1|p' <<<"\$expr")
else
  dir=\${ref%%#*}
  commit=\$(git -C "\$dir" rev-parse HEAD)
  stamp="\"\$commit\""
fi
echo "/nix/store/\$(printf '%s\n' "\$(git -C "\$dir" ls-tree "\$commit" -- flake.lock system.nix)" "\$stamp" | sha256sum | cut -c1-32)-nixos-system"
EOF
chmod +x "$tmp/bin/nix"
PATH=$tmp/bin:$PATH

# What update.sh gives them, and its build.
# shellcheck disable=SC2034 # running.sh's
CONFIG=box state=$tmp/state work=$tmp/work riding='' changed=1 current='' rev=''
as_user() { "$@"; }
git_() { git -C "$work" "$@"; }
build() {
  as_user nix build --no-link --print-out-paths \
    "$work#nixosConfigurations.$CONFIG.config.system.build.toplevel" 2>"$state/logs/build.log" | tail -n1
}
# shellcheck source=/dev/null
. "$RUNNING"

fail() { echo "FAIL: $*" >&2; exit 1; }
# Commits on what's checked out, at time $1 (seconds into the test): $2 =
# file, $3 = its text.
at=1700000000
commit() {
  echo "$3" >"$work/$2"
  git_ add "$2"
  GIT_AUTHOR_DATE="@$((at + $1)) +0000" GIT_COMMITTER_DATE="@$((at + $1)) +0000" git_ commit -qm "$2: $3"
}
# Main moves, at time $1, as another PR's landing would: $2 = file, $3 = its
# text.
landed() {
  git_ checkout -q main
  commit "$@"
}
# HEAD as ride leaves it, at time $1: main with the PR's head (the branch pr)
# merged on top.
ride() {
  git_ checkout -q -B update main
  GIT_AUTHOR_DATE="@$((at + $1)) +0000" GIT_COMMITTER_DATE="@$((at + $1)) +0000" \
    git_ merge -q --no-edit -m "Ride #7 on main" pr
}
# What a run that put HEAD's system live leaves: it runs, stamped with HEAD.
live() {
  current=$(build)
  rev=$(git_ rev-parse HEAD)
}

mkdir -p "$state/logs"
git init -q "$work"
commit 0 flake.lock v1
commit 0 system.nix v1
commit 0 README v1
git_ checkout -q -b pr
commit 10 flake.lock v2 # the bump its PR holds
landed 20 README v2 # past the PR's base, so a ride is a merge

# The bug: riding the same PR on the same main a second time makes a new
# merge, so its build is a new store path though nothing in it differs. It
# runs already.
ride 30
live
ride 40
[ "$(git_ rev-parse HEAD)" != "$rev" ] || fail "the second ride is the first one's commit"
new=$(build)
[ "$new" != "$current" ] || fail "the stand-in for nix doesn't stamp a system with its commit"
runs_already "$new" || fail "a second ride of the same PR on the same main doesn't run already"
grep -q -- "--expr" "$tmp/nix.log" || fail "runs_already didn't build HEAD stamped as what runs"
echo "runs_already, ridden again: ok"

# Main moves under it with what doesn't go into the system (a README, a CI
# pin): it still runs already. With what does, it doesn't: that goes live.
landed 50 README v3
ride 60
runs_already "$(build)" || fail "a ride on a main moved only in its README doesn't run already"
landed 70 system.nix v2
ride 80
if runs_already "$(build)"; then fail "a ride on a main whose system moved runs already"; fi
echo "runs_already, main moved: ok"

# A bump of its own made again, the same lock (a run before put it live, but
# its PR didn't open or its push was refused): it runs already. A bump with
# more in it doesn't.
git_ checkout -q -B update main
commit 90 flake.lock v3
live
git_ reset -q --hard main
commit 100 flake.lock v3
runs_already "$(build)" || fail "the same bump made again doesn't run already"
git_ reset -q --hard main
commit 110 flake.lock v4
if runs_already "$(build)"; then fail "a newer bump runs already"; fi
echo "runs_already, a bump again: ok"

# HEAD as main itself: the run asked before building, so only the very
# system counts, and nix isn't asked again.
git_ checkout -q -B update main
live
landed 120 README v4
git_ checkout -q -B update main
new=$(build)
: >"$tmp/nix.log"
if changed='' runs_already "$new"; then fail "main restamped runs already without being asked"; fi
[ ! -s "$tmp/nix.log" ] || fail "runs_already asked nix about main itself: $(cat "$tmp/nix.log")"
changed='' runs_already "$current" || fail "the very system running doesn't run already"
echo "runs_already, main itself: ok"

# A comparison that can't be made never reads as running already: the run
# goes on to put it live, as before.
ride 130
new=$(build)
touch "$tmp/nix-down"
if runs_already "$new"; then fail "runs_already with nix failing said it runs already"; fi
rm "$tmp/nix-down"
echo "runs_already, nix failing: ok"

# Already live, a bump of its own: up only once HOST-update put that very
# system live (the switch and the health check), in a train repo or not;
# held when anyone else did; nothing to do while riding, or with nothing new.
rm -f "$state/deployed"
already=$(already_live /nix/store/sys-b)
[ "$already" = hold ] || fail "already_live with nothing deployed: $already"
echo /nix/store/sys-a >"$state/deployed"
already=$(already_live /nix/store/sys-b)
[ "$already" = hold ] || fail "already_live with another system deployed: $already"
echo /nix/store/sys-b >"$state/deployed"
already=$(already_live /nix/store/sys-b)
[ "$already" = up ] || fail "already_live with it deployed: $already"
already=$(TRAIN='' already_live /nix/store/sys-b)
[ "$already" = up ] || fail "already_live outside a train: $already"
already=$(riding=7 already_live /nix/store/sys-b)
[ "$already" = current ] || fail "already_live riding: $already"
already=$(changed='' already_live /nix/store/sys-b)
[ "$already" = current ] || fail "already_live with nothing new: $already"
echo "already_live: ok"

# What a run found live goes to burrow-dispatch whole: main's commit and the
# system it's in, as one JSON object, with nothing half written left behind.
live_at "$(git_ rev-parse main)" /nix/store/sys-b
[ "$(jq -c . "$state/live.json")" = "{\"main\":\"$(git_ rev-parse main)\",\"running\":\"/nix/store/sys-b\"}" ] ||
  fail "live_at wrote $(cat "$state/live.json")"
[ ! -e "$state/live.json.new" ] || fail "live_at left its temporary file"
echo "live_at: ok"

# Main runs: stamped with it, put live as it by a run whose system still
# runs, or the same apart from its stamp. Not once the box has been switched
# back since to an older main's system (a rollback, a trial boot's
# fallback), whatever the record of what a run put live says.
git_ checkout -q -B update main
live
main=$(git_ rev-parse main)
: >"$tmp/nix.log"
main_runs "$main" || fail "main_runs on a system stamped with main"
[ ! -s "$tmp/nix.log" ] || fail "main_runs asked nix about a system stamped with main"
old_current=$current old_rev=$rev
landed 140 system.nix v3
git_ checkout -q -B update main
live
main=$(git_ rev-parse main) main_current=$current main_rev=$rev
echo "$main" >"$state/deployed-main"
echo "$current" >"$state/deployed"
rev=$old_rev # a system stamped with something else, as one put up without a switch is
main_runs "$main" || fail "main_runs on main as a run put it live"
current=$old_current rev=$old_rev # rolled back to the older main's system
if main_runs "$main"; then fail "main_runs on a system rolled back from main"; fi
landed 150 README v5
git_ checkout -q -B update main
current=$main_current rev=$main_rev # main's system, stamped with an older commit of it
main_runs "$(git_ rev-parse main)" || fail "main_runs on main's system under an older stamp"
echo "main_runs: ok"

# How far main is live after a run: up to HEAD once it's on main as it went
# live, otherwise only up to the main the run started from (HEAD rebased
# before its push landed, a PR in a train repo, a push refused).
started=$(git_ rev-parse main)
git_ checkout -q -B update main
commit 160 flake.lock v5
git_ rev-parse HEAD >"$state/deployed-main"
[ "$(live_through "$started")" = "$(git_ rev-parse HEAD)" ] || fail "live_through with HEAD pushed as it went live"
echo "$started" >"$state/deployed-main"
[ "$(live_through "$started")" = "$started" ] || fail "live_through with HEAD not on main"
rm "$state/deployed-main"
[ "$(live_through "$started")" = "$started" ] || fail "live_through with nothing recorded"
echo "live_through: ok"
