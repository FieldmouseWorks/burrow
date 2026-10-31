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
unset GH_REPO
git config --global user.name test
git config --global user.email test@example.org
git config --global init.defaultBranch main
git config --global advice.detachedHead false

# gh: `api repos/R/pulls?...` answers the open PRs in $tmp/prs ("NUMBER HEAD
# [OWNER/REPO BRANCH]" lines, sam/homelab's burrow-update by default) as
# GitHub does, newest first and filtered by its head=OWNER:BRANCH, `pr create`
# adds one, `api graphql ... -F n=N` answers N's head with the checks in
# $tmp/checks (its check runs, statuses and check suites, as GraphQL has them;
# with $tmp/more there, the checks have a page more), `api repos/R/pulls/N -X
# PATCH` answers N's URL, and every call goes to $tmp/gh.log. With
# $tmp/gh-down there, it fails as an unreachable GitHub does. Like the
# machine's gh (gh-token), it runs a call only once it can tell the account
# the call is about: the repo a `repos/OWNER/...` endpoint or `-R` names,
# else GH_REPO's. `gh api graphql` names none, and gh-token refuses it.
mkdir "$tmp/bin"
cat >"$tmp/bin/gh" <<EOF
#!$(command -v bash)
set -euo pipefail
printf '%s\n' "\$*" >>"$tmp/gh.log"
owner=
for ((i = 1; i <= \$#; i++)); do
  case \${!i} in
    repos/*) owner=\${!i#repos/}; owner=\${owner%%/*} ;;
    -R) j=\$((i + 1)); owner=\${!j%%/*} ;;
  esac
done
[ -n "\$owner" ] || { owner=\${GH_REPO:-}; owner=\${owner%%/*}; }
[ -n "\$owner" ] || { echo "gh-token: which account is \\\`gh \$1 \$2\\\` for? Give it -R OWNER/REPO" >&2; exit 1; }
[ "\$owner" = sam ] || { echo "gh stand-in: a call for \$owner, not sam" >&2; exit 2; }
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
  "api graphql") n=; for a in "\$@"; do [ "\${a#n=}" = "\$a" ] || n=\${a#n=}; done
    json=\$(jq -Rn --arg n "\$n" --slurpfile checks "$tmp/checks" --argjson more "\$([ -e "$tmp/more" ] && echo true || echo false)" '
      [inputs | split(" ") | select(.[0] == \$n)][0][1] as \$head | \$checks[0] as \$c
      | {data: {repository: {pullRequest: {headRefOid: \$head, commits: {nodes: [{commit: {
          statusCheckRollup: {contexts: {pageInfo: {hasNextPage: \$more}, nodes: [\$c[] | select(.__typename != "CheckSuite")]}},
          checkSuites: {pageInfo: {hasNextPage: false}, nodes: [\$c[] | select(.__typename == "CheckSuite")]}}}]}}}}}' <"$tmp/prs") ;;
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
REPO=sam/homelab BRANCH=burrow-update HOST=box UPDATE=box-update
# shellcheck disable=SC2034 # propose's
SETTLED=review-settled STEWARD=sams-steward TRAIN_CONTEXT=conary/train
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
  rm -rf "$tmp/origin.git" "$work" "$state" "$tmp/gh-down" "$tmp/more"
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
url=$(propose "deps: update nixpkgs" "body text") || fail "propose failed"
[ "$url" = "https://github.com/sam/homelab/pull/7" ] || fail "propose's PR: $url"
head=$(git_ rev-parse HEAD)
[ "$(git -C "$tmp/origin.git" rev-parse refs/heads/burrow-update)" = "$head" ] || fail "the branch isn't HEAD"
[ "$(cat "$state/proposed")" = "$head" ] || fail "proposed isn't HEAD"
grep -qx -- "pr create -R sam/homelab --head burrow-update --base main --title deps: update nixpkgs --body body text" "$tmp/gh.log" ||
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

# A check run ($1 = its status, $2 = its conclusion), a status ($1 = its
# context, $2 = its state, $3 = who posted it, the steward's App's Bot by
# default, $4 = as what) and a check suite ($1 = its conclusion), as GraphQL
# has them.
run() { jq -n --arg s "$1" --arg c "$2" '{__typename: "CheckRun", status: $s, conclusion: (if $c == "" then null else $c end)}'; }
status() {
  jq -n --arg c "$1" --arg s "$2" --arg by "${3:-sams-steward}" --arg as "${4:-Bot}" \
    '{__typename: "StatusContext", context: $c, state: $s, creator: {login: $by, __typename: $as}}'
}
suite() { jq -n --arg c "$1" '{__typename: "CheckSuite", conclusion: $c}'; }
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

# Red, as the steward reads CI: a check on its own head failed, a workflow
# failed to start, a status failed, or the train found the head red alone
# (its failure, from the steward's App); it's fetched, for why_not_replace.
# Green, pending, rerunning, the steward's own status, the train's error (a
# merge GitHub refused), its failure from anyone else, over 100 checks, or
# someone else's head: nothing, so it's ridden (or held) as before. GitHub down, or a
# head that moved since it was listed: a failure, never "not red".
fresh
pushed_before v2
pr=$(update_pr)
head=${pr#* }
for green in "" "$(run IN_PROGRESS '')" "$(run COMPLETED SUCCESS) $(run COMPLETED SKIPPED) $(run COMPLETED NEUTRAL)" \
  "$(status ci/proof PENDING) $(status conary/train SUCCESS)" "$(status review-settled FAILURE)" \
  "$(status conary/train ERROR)" "$(status conary/train FAILURE mallory User)" "$(status conary/train FAILURE sams-steward User)" \
  "$(status conary/train FAILURE other-app)" "$(suite SUCCESS) $(run COMPLETED SUCCESS)"; do
  checks "$green"
  out=$(red_head "$pr") && [ -z "$out" ] || fail "red_head with $(jq -c . "$tmp/checks"): $out"
done
! git_ cat-file -e "$head^{tree}" 2>/dev/null || fail "red_head fetched a head that isn't red"
for red in "$(run COMPLETED FAILURE)" "$(run COMPLETED TIMED_OUT)" "$(run COMPLETED CANCELLED) $(run COMPLETED SUCCESS)" \
  "$(status conary/train FAILURE)" "$(status ci/proof ERROR)" "$(status ci/proof FAILURE mallory User)" \
  "$(suite STARTUP_FAILURE)" "$(suite STARTUP_FAILURE) $(suite SUCCESS) $(run COMPLETED SUCCESS)"; do
  checks "$red"
  out=$(red_head "$pr") && [ "$out" = red ] || fail "red_head with $(jq -c . "$tmp/checks"): $out"
done
git_ cat-file -e "$head^{tree}" || fail "red_head didn't fetch the red head"
touch "$tmp/more"
out=$(red_head "$pr") && [ -z "$out" ] || fail "red_head with over 100 checks, one red: $out"
rm "$tmp/more"
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

# A fresh bump replaces the red head only whole, and other than it: not the
# red head's very tree (nothing moved), nor main itself with nothing new on it
# (all its inputs failed, or none could be fetched, or none moved), nor one
# short of an input; once main moved, a whole one does.
fresh
pushed_before v2
pr=$(update_pr)
checks "$(run COMPLETED FAILURE)"
red_head "$pr" >/dev/null
at_main
main=$(git_ rev-parse HEAD)
commit flake.lock v2
why=$(why_not_replace "$pr" "$main") || fail "why_not_replace let the same bump replace it"
[ "$why" = "a fresh bump is that same tree" ] || fail "why_not_replace's word on the same bump: $why"
landed README new
at_main
main=$(git_ rev-parse HEAD)
why=$(why_not_replace "$pr" "$main") || fail "why_not_replace let main itself replace it"
[ "$why" = "a fresh bump has nothing new on main" ] || fail "why_not_replace's word on main itself: $why"
why=$(why_not_replace "$pr" "$main" claude-code nixpkgs) || fail "why_not_replace let main itself, short of every input, replace it"
[ "$why" = "a fresh bump is short of claude-code nixpkgs" ] || fail "why_not_replace's word on main short of every input: $why"
commit flake.lock v3
why=$(why_not_replace "$pr" "$main" claude-code) || fail "why_not_replace let a bump short of claude-code replace it"
[ "$why" = "a fresh bump is short of claude-code" ] || fail "why_not_replace's word on a bump short of an input: $why"
if why=$(why_not_replace "$pr" "$main"); then fail "why_not_replace held a whole, new bump: $why"; fi
echo "why_not_replace: ok"

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
