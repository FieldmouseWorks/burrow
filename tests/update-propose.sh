# HOST-update's PR in a train repo (modules/updates/propose.sh), on its own:
# real git, with a bare repo for GitHub's copy, and a stand-in for gh that
# answers from a file of open PRs and runs --jq through jq, as gh does.
#
#     bash tests/update-propose.sh modules/updates/propose.sh
set -euo pipefail

PROPOSE=$(realpath "$1")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export HOME=$tmp GIT_CONFIG_NOSYSTEM=1
git config --global user.name test
git config --global user.email test@example.org
git config --global init.defaultBranch main
git config --global advice.detachedHead false

# gh: `pr list` answers the open PRs in $tmp/prs ("NUMBER HEAD" lines) as
# JSON, `pr create` adds one, and every call goes to $tmp/gh.log. With
# $tmp/gh-down there, it fails as an unreachable GitHub does.
mkdir "$tmp/bin"
cat >"$tmp/bin/gh" <<EOF
#!$(command -v bash)
set -euo pipefail
printf '%s\n' "\$*" >>"$tmp/gh.log"
[ ! -e "$tmp/gh-down" ] || { echo "error connecting to api.github.com" >&2; exit 1; }
jq=
for ((i = 1; i <= \$#; i++)); do [ "\${!i}" != --jq ] || { j=\$((i + 1)); jq=\${!j}; }; done
case "\$1 \$2" in
  "pr list") json=\$(jq -Rn '[inputs | split(" ") | {number: (.[0] | tonumber), headRefOid: .[1]}]' <"$tmp/prs") ;;
  "pr create") n=\$((\$(wc -l <"$tmp/prs") + 7)); echo "\$n \$(git -C "$tmp/origin.git" rev-parse refs/heads/burrow-update)" >>"$tmp/prs"
    echo "https://github.com/sam/homelab/pull/\$n"; exit 0 ;;
  *) echo "gh stand-in: \$*?" >&2; exit 2 ;;
esac
if [ -n "\$jq" ]; then jq -r "\$jq" <<<"\$json"; else echo "\$json"; fi
EOF
chmod +x "$tmp/bin/gh"
PATH=$tmp/bin:$PATH

# What update.sh gives them.
# shellcheck disable=SC2034 # propose's
REPO=sam/homelab BRANCH=burrow-update HOST=box UPDATE=box-update TRAIN=1 riding='' changed=1
state=$tmp/state work=$tmp/work
as_user() { "$@"; }
git_() { git -C "$work" "$@"; }
# shellcheck source=/dev/null
. "$PROPOSE"

fail() { echo "FAIL: $*" >&2; exit 1; }
commit() { # $1 = file, $2 = its text, in the work tree
  echo "$2" >"$work/$1"
  git_ add "$1"
  git_ commit -qm "$1: $2" -m "Pushed by $UPDATE after the build, the VM tests and the health check passed."
}

# A fresh GitHub copy with one commit on main, a clone of it at main, and no
# PRs; HOST-update has proposed nothing.
fresh() {
  rm -rf "$tmp/origin.git" "$work" "$state" "$tmp/gh-down"
  mkdir -p "$state"
  : >"$tmp/prs"
  : >"$tmp/gh.log"
  git init -q --bare "$tmp/origin.git"
  git clone -q "$tmp/origin.git" "$work" 2>/dev/null
  commit flake.lock v1
  git_ push -q origin HEAD:main
}
# main moves on GitHub as another PR's landing would: $1 = file, $2 = text.
landed() {
  local other=$tmp/other
  rm -rf "$other"
  git clone -q "$tmp/origin.git" "$other"
  echo "$2" >"$other/$1"
  git -C "$other" add "$1"
  git -C "$other" commit -qm "$1: $2"
  git -C "$other" push -q origin HEAD:main
}
# The clone as update.sh leaves it before riding: at GitHub's main.
at_main() {
  git_ fetch -q origin main
  git_ checkout -q -B update origin/main
  git_ reset -q --hard origin/main
}

# Nothing open: nothing to say. Open: its number and head. GitHub down: a
# failure, never "none open".
fresh
out=$(update_pr) && [ -z "$out" ] || fail "update_pr with none open: $out"
echo "7 abc" >"$tmp/prs"
out=$(update_pr) && [ "$out" = "7 abc" ] || fail "update_pr with #7 open: $out"
grep -q -- "pr list -R sam/homelab --head burrow-update --base main --state open" "$tmp/gh.log" ||
  fail "update_pr asked about another PR: $(cat "$tmp/gh.log")"
touch "$tmp/gh-down"
! update_pr >/dev/null 2>&1 || fail "update_pr with GitHub down said none open"
echo "update_pr: ok"

# Proposing: HEAD goes to the branch, over whatever was there, and is what
# HOST-update remembers it pushed; the PR is opened against main.
fresh
commit flake.lock old # an old PR's head, never landed
git_ push -q origin HEAD:refs/heads/burrow-update
git_ reset -q --hard HEAD~1
commit flake.lock v2
url=$(propose "Update nixpkgs" "body text") || fail "propose failed"
[ "$url" = "https://github.com/sam/homelab/pull/7" ] || fail "propose's PR: $url"
head=$(git_ rev-parse HEAD)
[ "$(git -C "$tmp/origin.git" rev-parse refs/heads/burrow-update)" = "$head" ] || fail "the branch isn't HEAD"
[ "$(cat "$state/proposed")" = "$head" ] || fail "proposed isn't HEAD"
grep -qx -- "pr create -R sam/homelab --head burrow-update --base main --title Update nixpkgs --body body text" "$tmp/gh.log" ||
  fail "propose's gh: $(cat "$tmp/gh.log")"
[ "$(git -C "$tmp/origin.git" rev-parse refs/heads/main)" != "$head" ] || fail "propose pushed to main"
echo "propose: ok"

# Its body: the commit's own word on what it passed, and that it's live.
body=$(pr_body)
grep -q "after the build, the VM tests and the health check passed" <<<"$body" || fail "pr_body: $body"
grep -q "It is live on box" <<<"$body" || fail "pr_body: $body"
echo "pr_body: ok"

# Riding, main where the PR left it: HEAD is the PR's head.
pr=$(update_pr)
at_main
ride "$pr" || fail "ride on an unmoved main"
[ "$(git_ rev-parse HEAD)" = "$head" ] || fail "riding an unmoved main isn't the PR's head"

# Riding, main moved on: main with the PR on top.
landed README new
at_main
main=$(git_ rev-parse HEAD)
ride "$pr" || fail "ride on a moved main"
git_ merge-base --is-ancestor "$main" HEAD || fail "riding lost main"
git_ merge-base --is-ancestor "$head" HEAD || fail "riding lost the PR"
[ "$(cat "$work/flake.lock")" = v2 ] && [ "$(cat "$work/README")" = new ] || fail "riding's tree"
echo "ride: ok"

# Someone else's head on it (a fix job's): no riding, HEAD still main.
fresh
commit flake.lock v2
propose t b >/dev/null
echo v3 >"$work/flake.lock"
git_ commit -qam "fix it"
git_ push -q origin HEAD:refs/heads/burrow-update
sed -i "s/ .*/ $(git_ rev-parse HEAD)/" "$tmp/prs"
at_main
main=$(git_ rev-parse HEAD)
if why=$(ride "$(update_pr)"); then fail "rode a head it didn't push"; fi
grep -q "a head it didn't push" <<<"$why" || fail "ride's word on a head it didn't push: $why"
[ "$(git_ rev-parse HEAD)" = "$main" ] || fail "a refused ride moved HEAD"
echo "ride, not its head: ok"

# GitHub's branch moved since gh listed the PR: no riding what wasn't read.
fresh
commit flake.lock v2
propose t b >/dev/null
listed=$(update_pr)
echo v3 >"$work/flake.lock"
git_ commit -qam "pushed since"
git_ push -q origin HEAD:refs/heads/burrow-update
at_main
main=$(git_ rev-parse HEAD)
if why=$(ride "$listed"); then fail "rode a branch that moved since it was listed"; fi
grep -q "couldn't fetch" <<<"$why" || fail "ride's word on a moved branch: $why"
[ "$(git_ rev-parse HEAD)" = "$main" ] || fail "a refused ride moved HEAD"
echo "ride, moved branch: ok"

# A PR that no longer merges onto main: no riding, and no merge left half done.
fresh
commit flake.lock v2
propose t b >/dev/null
landed flake.lock other
at_main
main=$(git_ rev-parse HEAD)
if why=$(ride "$(update_pr)"); then fail "rode a PR that conflicts with main"; fi
grep -q "doesn't merge onto main" <<<"$why" || fail "ride's word on a conflict: $why"
[ "$(git_ rev-parse HEAD)" = "$main" ] || fail "a conflicted ride moved HEAD"
[ ! -e "$work/.git/MERGE_HEAD" ] && [ -z "$(git_ status --porcelain)" ] || fail "a conflicted ride left a merge behind"
echo "ride, conflict: ok"

# Already live, a bump of its own: up only once HOST-update put that very
# system live (the switch and the health check); held when anyone else did;
# nothing to do outside a train, while riding, or with nothing new.
fresh
already=$(already_live /nix/store/sys-b)
[ "$already" = hold ] || fail "already_live with nothing deployed: $already"
echo /nix/store/sys-a >"$state/deployed"
already=$(already_live /nix/store/sys-b)
[ "$already" = hold ] || fail "already_live with another system deployed: $already"
echo /nix/store/sys-b >"$state/deployed"
already=$(already_live /nix/store/sys-b)
[ "$already" = up ] || fail "already_live with it deployed: $already"
already=$(TRAIN='' already_live /nix/store/sys-b)
[ "$already" = current ] || fail "already_live outside a train: $already"
already=$(riding=7 already_live /nix/store/sys-b)
[ "$already" = current ] || fail "already_live riding: $already"
already=$(changed='' already_live /nix/store/sys-b)
[ "$already" = current ] || fail "already_live with nothing new: $already"
echo "already_live: ok"
