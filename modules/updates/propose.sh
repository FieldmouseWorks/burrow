# HOST-update's PR, in a repo whose PRs land through its merge train
# (TRAIN set): nothing but the train moves its main, so the tested commit goes
# up as a PR from the branch BRANCH, for the steward to settle and the train
# to land (Conary's docs/DESIGN.md, section 4.5), never as a push. These are
# update.sh's own, defined ahead of it; tests/update-propose.sh runs them
# alone.
#
# While that PR is open, HOST-update brings in nothing fresh. A new head
# would take it out of the train, and stop the candidate it's in and every
# PR in that with it; a newer bump put live beside it would leave the box on
# something main never gets. So it rides the PR instead: main with the PR's
# head on top, which is what the train lands next.
#
# Unless a check failed on a head it pushed itself (red_head). The steward
# settles only a green PR, and takes one that goes red out of the train, so
# that head is in no candidate for a new one to stop, and riding it would
# hold the box until a Claude job got to it. So it brings in a fresh bump as
# if none were open, which goes up as the same PR's new head (replace):
# only a whole one, though, that isn't the red head's very tree
# (why_not_replace).

# update.sh sets state, REPO, BRANCH, HOST, UPDATE, SETTLED, STEWARD,
# TRAIN_CONTEXT, as_user and git_.
# shellcheck disable=SC2154

# gh, about REPO. The machine's gh (gh-token) runs a call with the token of
# the account it's about, and `gh api graphql` names none: GH_REPO does.
gh_() { as_user env GH_REPO="$REPO" gh "$@"; }

# The open PR, as "NUMBER HEAD"; nothing when there's none. Fails when GitHub
# doesn't say. Only REPO's own BRANCH counts: a fork's PR from a branch of the
# same name is anyone's to open. GitHub's head filter is OWNER:BRANCH, and
# allows one open PR per head and base, so there's never a page more.
update_pr() {
  gh_ api "repos/$REPO/pulls?state=open&base=main&head=${REPO%%/*}:$BRANCH" \
    --jq '.[0] // empty | "\(.number) \(.head.sha)"'
}

# Whether its open PR $1 ("NUMBER HEAD") is red on a head HOST-update
# pushed: "red" when it is, with that head fetched (why_not_replace reads
# it), and nothing when it isn't, or when the head is anyone else's (ride
# holds for that). Red is CI as the steward reads it (ci_state in ../autoland/prs.py,
# whose rules these are): a check whose latest run failed (so one rerunning
# is pending), a workflow that failed to start, or a status that's neither
# success nor pending, except the steward's own (SETTLED) and the train's
# (TRAIN_CONTEXT), which counts only as a failure from the steward's App
# (STEWARD): the head red merged alone. Over 100 checks or suites isn't red:
# the steward doesn't read past them either. Fails, saying why, when GitHub
# doesn't say, or the head moved since $1 was read.
red_head() {
  local n head json red
  read -r n head <<<"$1"
  [ "$head" = "$(cat "$state/proposed" 2>/dev/null)" ] || return 0
  # shellcheck disable=SC2016 # GraphQL's variables
  json=$(gh_ api graphql -F n="$n" -f owner="${REPO%%/*}" -f name="${REPO#*/}" -f query='
    query($owner: String!, $name: String!, $n: Int!) {
      repository(owner: $owner, name: $name) { pullRequest(number: $n) { headRefOid
        commits(last: 1) { nodes { commit {
          statusCheckRollup { contexts(first: 100) { pageInfo { hasNextPage } nodes {
            __typename
            ... on CheckRun { status conclusion }
            ... on StatusContext { context state creator { login __typename } }
          } } }
          checkSuites(first: 100) { pageInfo { hasNextPage } nodes { conclusion } }
        } } }
      } }
    }') || {
    echo "GitHub didn't say how its PR #$n's checks stand"
    return 1
  }
  red=$(jq -r --arg head "$head" --arg settled "$SETTLED" --arg train "$TRAIN_CONTEXT" --arg steward "$STEWARD" '
    .data.repository.pullRequest as $pr | ($pr.commits.nodes[0].commit // {}) as $c
    | if $pr.headRefOid != $head then error("moved") else
      ($c.statusCheckRollup.contexts // {}) as $contexts | ($c.checkSuites // {}) as $suites
      | if [$contexts, $suites] | any(.pageInfo.hasNextPage) then empty else
        [($suites.nodes[]? | .conclusion == "STARTUP_FAILURE"),
         ($contexts.nodes[]?
          | if .__typename == "CheckRun" then .status == "COMPLETED" and (.conclusion | IN("SUCCESS", "NEUTRAL", "SKIPPED") | not)
            elif .context == $settled then false
            elif .context == $train then .state == "FAILURE" and .creator.__typename == "Bot" and .creator.login == $steward
            else .state | IN("SUCCESS", "PENDING", "EXPECTED") | not end)]
        | if any then "red" else empty end
      end
    end' <<<"$json" 2>/dev/null) || {
    echo "its PR #$n's head isn't $head any more"
    return 1
  }
  [ -n "$red" ] || return 0
  git_ fetch -q origin "refs/heads/$BRANCH" && [ "$(git_ rev-parse FETCH_HEAD)" = "$head" ] || {
    echo "couldn't fetch its PR #$n's head ($head)"
    return 1
  }
  echo red
}

# Puts the open PR $1 ("NUMBER HEAD") on top of main. Fails, saying why, when
# that head isn't the one HOST-update pushed (someone's fixing it: what it
# holds hasn't been through HOST-update's gate) or doesn't merge onto main.
ride() {
  local n head
  read -r n head <<<"$1"
  if [ "$head" != "$(cat "$state/proposed" 2>/dev/null)" ]; then
    echo "its PR #$n has a head it didn't push ($head); holding until that lands"
    return 1
  fi
  git_ fetch -q origin "refs/heads/$BRANCH" && [ "$(git_ rev-parse FETCH_HEAD)" = "$head" ] || {
    echo "couldn't fetch its PR #$n's head ($head)"
    return 1
  }
  git_ merge -q --no-edit -m "Ride #$n on main" FETCH_HEAD >/dev/null 2>&1 || {
    git_ merge --abort 2>/dev/null || git_ reset -q --hard
    echo "its PR #$n doesn't merge onto main; holding until it's fixed"
    return 1
  }
}

# Why HEAD, a fresh bump on main $2 for its red PR $1, doesn't replace that
# head, when it doesn't; $3... are the inputs that didn't make it in (they
# didn't build, or what's new in them couldn't be fetched). One short of any,
# or with nothing new on main at all, could take back what that head put
# live; one that's that head's very tree would only go red the same way
# again. Either way the Claude job on the red PR (or a rerun) has it.
why_not_replace() {
  local pr=$1 main=$2 tree
  shift 2
  tree=$(git_ rev-parse 'HEAD^{tree}')
  if [ $# -gt 0 ]; then
    echo "a fresh bump is short of $*"
  elif [ "$tree" = "$(git_ rev-parse "$main^{tree}")" ]; then
    echo "a fresh bump has nothing new on main"
  elif [ "$tree" = "$(git_ rev-parse "${pr#* }^{tree}")" ]; then
    echo "a fresh bump is that same tree"
  else
    return 1
  fi
}

# HEAD as its red PR $1's new head, retitled $2 with body $3: the same PR,
# never a new one. Only while that head is still on it and still red: a head
# pushed since is someone's fix, and one gone green (a rerun) is the
# steward's to settle and put in the train, so it's ridden instead. Prints
# the PR's URL; fails, saying why, with the PR left as it was. update.sh
# calls it once HEAD is live and healthy but before it's confirmed, so a
# refusal reverts it: the box never stays on a bump nothing will land.
replace() {
  local n head red url
  read -r n head <<<"$1"
  red=$(red_head "$1") || { echo "$red; its PR #$n stays as it is"; return 1; }
  [ -n "$red" ] || { echo "its PR #$n isn't red any more, so it stays as it is"; return 1; }
  git_ push -q --force-with-lease="refs/heads/$BRANCH:$head" origin "HEAD:refs/heads/$BRANCH" 2>/dev/null || {
    echo "its PR #$n moved on from $head while this ran, so it stays as it is"
    return 1
  }
  git_ rev-parse HEAD >"$state/proposed"
  url=$(gh_ api "repos/$REPO/pulls/$n" -X PATCH -f "title=$2" -f "body=$3" --jq .html_url) ||
    url="https://github.com/$REPO/pull/$n (its title still says the red head's: GitHub didn't take the new one)"
  echo "$url, in place of its red head ${head:0:7}"
}

# HEAD as a new PR: title $1, body $2. Only with none open: whatever was on
# BRANCH (a PR merged or closed) is HOST-update's to replace.
propose() {
  git_ push -q --force origin "HEAD:refs/heads/$BRANCH" || return 1
  git_ rev-parse HEAD >"$state/proposed"
  gh_ pr create -R "$REPO" --head "$BRANCH" --base main --title "$1" --body "$2"
}

# The PR's body: what HOST-update did with the commit, as its message says.
pr_body() {
  printf '%s\n\nIt is live on %s. The steward settles this PR and the merge train lands it; until then %s brings in nothing newer, unless a check fails on it: then a fresh bump replaces it.\n' \
    "$(git_ log -1 --format=%b)" "$HOST" "$UPDATE"
}
