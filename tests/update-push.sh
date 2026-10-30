# The update job's push (modules/updates/push.sh), on real git repos: an update
# that went live reaches main even when main moved while it ran. $1 is push.sh.
set -euo pipefail

tmp=$(mktemp -d)
state=$tmp/state
mkdir -p "$state"
export HOME=$tmp GIT_CONFIG_NOSYSTEM=1
git config --global user.name update
git config --global user.email update@example.com
git config --global init.defaultBranch main

git init -q --bare "$tmp/origin.git"
git clone -q "$tmp/origin.git" "$tmp/seed" 2>/dev/null
printf 'one\ntwo\nthree\nfour\nfive\n' >"$tmp/seed/flake.lock"
echo a >"$tmp/seed/module.nix"
git -C "$tmp/seed" add -A
git -C "$tmp/seed" commit -qm seed
git -C "$tmp/seed" push -q origin HEAD:main
git clone -q "$tmp/origin.git" "$tmp/work"
git clone -q "$tmp/origin.git" "$tmp/other"

# Pushes to refuse: before each of the next $moves pushes, something lands.
moves=0
git_() {
  if [ "$1" = push ] && [ "$moves" -gt 0 ]; then
    moves=$((moves - 1))
    land "module.nix" "landed-$(git -C "$tmp/other" rev-parse HEAD)"
  fi
  git -C "$tmp/work" "$@"
}
# The system's build: which trees it ran on, and whether it fails.
unbuildable=
build() {
  git -C "$tmp/work" rev-parse 'HEAD^{tree}' >>"$tmp/built"
  [ -n "$unbuildable" ] || echo /nix/store/system
}
# shellcheck source=/dev/null
. "$1"

fail() { echo "FAIL: $*" >&2; exit 1; }
land() {
  git -C "$tmp/other" pull -q --ff-only origin main
  echo "$2" >"$tmp/other/$1"
  git -C "$tmp/other" commit -qam "$2"
  git -C "$tmp/other" push -q origin HEAD:main
}
on_main() { git -C "$tmp/origin.git" show "main:$1"; }
# A bump on top of a clean copy of main, as update.sh's run has it.
bump() {
  git -C "$tmp/work" fetch -q origin main
  git -C "$tmp/work" checkout -q -B update origin/main
  sed -i "1s/.*/$1/" "$tmp/work/flake.lock"
  git -C "$tmp/work" commit -qam "Update $1"
}
rm_deployed() { rm -f "$state/deployed-main"; }

# Nothing moved: the commit that went live is main, and main runs it.
bump one-a
push_main >/dev/null 2>&1 || fail "a plain push failed"
[ "$(on_main flake.lock | head -1)" = one-a ] || fail "the bump isn't on main"
[ "$(cat "$state/deployed-main")" = "$(git -C "$tmp/origin.git" rev-parse main)" ] || fail "deployed-main isn't main"

# Main moved during the run: the bump goes on top of what landed, and main isn't
# marked as running what landed, so the next run deploys it.
rm_deployed
bump one-b
moves=1
push_main >/dev/null 2>&1 || fail "a push after main moved failed"
[ "$(on_main flake.lock | head -1)" = one-b ] || fail "the bump didn't reach main after main moved"
[ "$(on_main module.nix)" != a ] || fail "what landed is gone from main"
[ ! -e "$state/deployed-main" ] || fail "main is marked as running what it hasn't deployed"
[ "$(tail -1 "$tmp/built")" = "$(git -C "$tmp/origin.git" rev-parse 'main^{tree}')" ] || fail "what went on main wasn't what was built"

# What landed doesn't build with the bump: main stays as it is.
bump one-bad
unbuildable=1 moves=1
before_landing=$(git -C "$tmp/origin.git" show main:flake.lock | head -1)
if out=$(push_main 2>/dev/null); then fail "an update that doesn't build claims it was pushed"; fi
unbuildable=
case $out in *"next run"*) ;; *) fail "the failed build isn't explained: $out" ;; esac
[ "$(on_main flake.lock | head -1)" = "$before_landing" ] || fail "an update that doesn't build reached main"

# Main moved twice over: it goes again.
bump one-c
moves=2
push_main >/dev/null 2>&1 || fail "a push after main moved twice failed"
[ "$(on_main flake.lock | head -1)" = one-c ] || fail "the bump didn't reach main after main moved twice"
[ ! -e "$state/deployed-main" ] || fail "main is marked as running what it hasn't deployed"

# A run with nothing to bump, deploying main itself, when main moved: nothing to
# add, and main still isn't what it deployed.
git -C "$tmp/work" fetch -q origin main
git -C "$tmp/work" checkout -q -B update origin/main
moves=1 before_build=$(wc -l <"$tmp/built")
push_main >/dev/null 2>&1 || fail "deploying main that then moved failed"
[ ! -e "$state/deployed-main" ] || fail "main is marked as running what landed after the deploy"
[ "$(wc -l <"$tmp/built")" = "$before_build" ] || fail "it built with nothing of its own to publish"

# What landed touched the same lines: main is left as it is, nothing half done,
# and the failure says so.
bump one-d
land flake.lock "one-e"
before=$(git -C "$tmp/origin.git" rev-parse main)
if out=$(push_main 2>/dev/null); then fail "a conflicting bump claims it was pushed"; fi
case $out in *"next run"*) ;; *) fail "the conflict isn't explained: $out" ;; esac
[ "$(git -C "$tmp/origin.git" rev-parse main)" = "$before" ] || fail "a conflict changed main"
[ ! -d "$tmp/work/.git/rebase-merge" ] && [ ! -d "$tmp/work/.git/rebase-apply" ] || fail "a rebase was left in progress"
[ ! -e "$state/deployed-main" ] || fail "a conflict marked main as deployed"

echo "update-push: ok"
