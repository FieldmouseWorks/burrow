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

# gh: `api repos/R/pulls?...` answers the open PRs in $tmp/prs ("NUMBER HEAD
# [OWNER/REPO BRANCH]" lines, sam/homelab's burrow-update by default) as
# GitHub does, newest first and filtered by its head=OWNER:BRANCH, `pr create`
# adds one, `pr view N` answers N's head with the checks in $tmp/checks (its
# statusCheckRollup, as gh gives it), `api repos/R/pulls/N -X PATCH` answers
# N's URL, and every call goes to $tmp/gh.log. With $tmp/gh-down there, it
# fails as an unreachable GitHub does.
mkdir "$tmp/bin"
cat >"$tmp/bin/gh" <<EOF
#!$(command -v bash)
set -euo pipefail
printf '%s\n' "\$*" >>"$tmp/gh.log"
[ ! -e "$tmp/gh-down" ] || { echo "error connecting to api.github.com" >&2; exit 1; }
jq=
for ((i = 1; i <= \$#; i++)); do [ "\${!i}" != --jq ] || { j=\$((i + 1)); jq=\${!j}; }; done
case "\$1 \$2" in
  "api repos/sam/homelab/pulls?"*)
    head=\$(sed -n 's/.*[?&]head=\([^&]*\).*/\1/p' <<<"\$2")
    json=\$(jq -Rn --arg head "\$head" '[inputs | split(" ")
      | {number: (.[0] | tonumber), sha: .[1], repo: (.[2] // "sam/homelab"), ref: (.[3] // "burrow-update")}
      | select(\$head == "" or \$head == "\(.repo | split("/")[0]):\(.ref)")
      | {number, head: {sha, ref, repo: {full_name: .repo}}}] | reverse' <"$tmp/prs") ;;
  "pr view") json=\$(jq -Rn --arg n "\$3" --slurpfile checks "$tmp/checks" '[inputs | split(" ") | select(.[0] == \$n)][0]
      | {headRefOid: .[1], statusCheckRollup: \$checks[0]}' <"$tmp/prs") ;;
  "api repos/sam/homelab/pulls/"*) json=\$(jq -n --arg url "https://github.com/sam/homelab/pull/\${2##*/}" '{html_url: \$url}') ;;
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
REPO=sam/homelab BRANCH=burrow-update HOST=box UPDATE=box-update TRAIN=1 SETTLED=review-settled riding='' changed=1
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
  echo '[]' >"$tmp/checks"
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
grep -q -- "api repos/sam/homelab/pulls?state=open&base=main&head=sam:burrow-update" "$tmp/gh.log" ||
  fail "update_pr asked about another PR: $(cat "$tmp/gh.log")"
# A fork's PR from a branch of the same name, newer than its own or alone:
# never its own.
echo "9 def mallory/homelab burrow-update" >>"$tmp/prs"
out=$(update_pr) && [ "$out" = "7 abc" ] || fail "update_pr with a fork's same-named PR open too: $out"
echo "9 def mallory/homelab burrow-update" >"$tmp/prs"
out=$(update_pr) && [ -z "$out" ] || fail "update_pr with only a fork's same-named PR open: $out"
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

# A check run ($1 = its status, $2 = its conclusion) and a status ($1 = its
# context, $2 = its state), as gh's statusCheckRollup has them.
run() { jq -n --arg s "$1" --arg c "$2" '{__typename: "CheckRun", name: "check / check", status: $s, conclusion: (if $c == "" then null else $c end)}'; }
status() { jq -n --arg c "$1" --arg s "$2" '{__typename: "StatusContext", context: $c, state: $s}'; }
# The head's checks are these (each a JSON object).
checks() { jq -s . <<<"$*" >"$tmp/checks"; }

# A head of its own, pushed by a run before this one from a clone since gone:
# GitHub's branch has it and this clone doesn't, until it's fetched.
pushed_before() { # $1 = flake.lock's text in it
  local other=$tmp/before
  rm -rf "$other"
  git clone -q "$tmp/origin.git" "$other"
  echo "$1" >"$other/flake.lock"
  git -C "$other" commit -qam "flake.lock: $1"
  git -C "$other" push -q --force origin HEAD:refs/heads/burrow-update
  git -C "$other" rev-parse HEAD >"$state/proposed"
  echo "7 $(git -C "$other" rev-parse HEAD)" >"$tmp/prs"
}

# Red: a check on its own head failed, or the train turned it away; it's
# fetched, for same_tree. Green, pending, rerunning, the steward's own status
# or someone else's head: nothing, so it's ridden (or held) as before. GitHub
# down, or a head that moved since it was listed: a failure, never "not red".
fresh
pushed_before v2
pr=$(update_pr)
head=${pr#* }
for green in "" "$(run IN_PROGRESS '')" "$(run COMPLETED SUCCESS) $(run COMPLETED SKIPPED) $(run COMPLETED NEUTRAL)" \
  "$(status ci/proof PENDING) $(status conary/train SUCCESS)" "$(status review-settled FAILURE)"; do
  checks "$green"
  out=$(red_head "$pr") && [ -z "$out" ] || fail "red_head with $(jq -c . "$tmp/checks"): $out"
done
! git_ cat-file -e "$head^{tree}" 2>/dev/null || fail "red_head fetched a head that isn't red"
for red in "$(run COMPLETED FAILURE)" "$(run COMPLETED TIMED_OUT)" "$(run COMPLETED CANCELLED) $(run COMPLETED SUCCESS)" \
  "$(status conary/train FAILURE)" "$(status ci/proof ERROR)"; do
  checks "$red"
  out=$(red_head "$pr") && [ "$out" = red ] || fail "red_head with $(jq -c . "$tmp/checks"): $out"
done
git_ cat-file -e "$head^{tree}" || fail "red_head didn't fetch the red head"
git_ rev-parse HEAD >"$state/proposed"
out=$(red_head "$pr") && [ -z "$out" ] || fail "red_head with someone else's head on it: $out"
echo "$head" >"$state/proposed"
touch "$tmp/gh-down"
! red_head "$pr" >/dev/null 2>&1 || fail "red_head with GitHub down said it isn't red"
rm "$tmp/gh-down"
listed=$pr
pushed_before v3 # GitHub's head moves on after the listing
echo "${listed#* }" >"$state/proposed"
if why=$(red_head "$listed"); then fail "red_head on a head that moved since it was listed: $why"; fi
grep -q "isn't ${listed#* } any more" <<<"$why" || fail "red_head's word on a moved head: $why"
echo "red_head: ok"

# A fresh bump for it is the red head's very tree when nothing moved, and
# isn't once main has.
fresh
pushed_before v2
pr=$(update_pr)
checks "$(run COMPLETED FAILURE)"
red_head "$pr" >/dev/null
at_main
commit flake.lock v2
same_tree "$pr" || fail "same_tree on the same bump"
landed README new
at_main
commit flake.lock v2
! same_tree "$pr" || fail "same_tree once main moved"
echo "same_tree: ok"

# Replacing: the fresh bump goes to the branch as the same PR's new head,
# retitled, and is what HOST-update remembers it pushed. No new PR.
url=$(replace "$pr" "Update nixpkgs" "body text") || fail "replace failed: $url"
[ "$url" = "https://github.com/sam/homelab/pull/7, in place of its red head ${pr:2:7}" ] || fail "replace's word: $url"
fresh_head=$(git_ rev-parse HEAD)
[ "$(git -C "$tmp/origin.git" rev-parse refs/heads/burrow-update)" = "$fresh_head" ] || fail "replace didn't move the branch"
[ "$(cat "$state/proposed")" = "$fresh_head" ] || fail "proposed isn't the fresh head"
grep -qx -- "api repos/sam/homelab/pulls/7 -X PATCH -f title=Update nixpkgs -f body=body text --jq .html_url" "$tmp/gh.log" ||
  fail "replace's gh: $(cat "$tmp/gh.log")"
! grep -q "pr create" "$tmp/gh.log" || fail "replace opened a new PR"
[ "$(git -C "$tmp/origin.git" rev-parse refs/heads/main)" != "$fresh_head" ] || fail "replace pushed to main"
echo "replace: ok"

# Green again since (a rerun): left as it was, to ride.
fresh
pushed_before v2
pr=$(update_pr)
checks "$(run IN_PROGRESS '')"
at_main
commit flake.lock v3
if why=$(replace "$pr" t b); then fail "replaced a head that isn't red: $why"; fi
grep -q "isn't red any more" <<<"$why" || fail "replace's word on a head that isn't red: $why"
[ "$(git -C "$tmp/origin.git" rev-parse refs/heads/burrow-update)" = "${pr#* }" ] || fail "replace moved a head that isn't red"
[ "$(cat "$state/proposed")" = "${pr#* }" ] || fail "replace changed proposed for a head that isn't red"
echo "replace, green again: ok"

# A fix pushed between its look and its push: the push is refused, and the
# fix stays.
fresh
pushed_before v2
pr=$(update_pr)
checks "$(run COMPLETED FAILURE)"
at_main
commit flake.lock v3
fix() {
  local other=$tmp/fix
  rm -rf "$other"
  git clone -q -b burrow-update "$tmp/origin.git" "$other" 2>/dev/null
  echo fixed >"$other/flake.lock"
  git -C "$other" commit -qam "fix it"
  git -C "$other" push -q origin HEAD:burrow-update
  git -C "$other" rev-parse HEAD
}
# shellcheck disable=SC2329 # replace calls it
git_() { [ "$1" != push ] || [ -n "${fixed:-}" ] || fixed=$(fix); git -C "$work" "$@"; }
if why=$(replace "$pr" t b); then fail "replace pushed over a fix: $why"; fi
git_() { git -C "$work" "$@"; }
grep -q "moved on from" <<<"$why" || fail "replace's word on a fix pushed meanwhile: $why"
[ "$(git -C "$tmp/origin.git" show refs/heads/burrow-update:flake.lock)" = fixed ] || fail "replace pushed over a fix"
[ "$(cat "$state/proposed")" = "${pr#* }" ] || fail "replace changed proposed when its push was refused"
! grep -q PATCH "$tmp/gh.log" || fail "replace retitled a PR it didn't push to"
echo "replace, a fix meanwhile: ok"
