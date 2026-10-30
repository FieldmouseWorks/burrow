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

# update.sh sets state, REPO, BRANCH, HOST, UPDATE, as_user and git_.
# shellcheck disable=SC2154

# The open PR, as "NUMBER HEAD"; nothing when there's none. Fails when GitHub
# doesn't say. Only REPO's own BRANCH counts: a fork's PR from a branch of the
# same name is anyone's to open. GitHub's head filter is OWNER:BRANCH, and
# allows one open PR per head and base, so there's never a page more.
update_pr() {
  as_user gh api "repos/$REPO/pulls?state=open&base=main&head=${REPO%%/*}:$BRANCH" \
    --jq '.[0] // empty | "\(.number) \(.head.sha)"'
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

# HEAD as a new PR: title $1, body $2. Only with none open: whatever was on
# BRANCH (a PR merged or closed) is HOST-update's to replace.
propose() {
  git_ push -q --force origin "HEAD:refs/heads/$BRANCH" || return 1
  git_ rev-parse HEAD >"$state/proposed"
  as_user gh pr create -R "$REPO" --head "$BRANCH" --base main --title "$1" --body "$2"
}

# The PR's body: what HOST-update did with the commit, as its message says.
pr_body() {
  printf '%s\n\nIt is live on %s. The steward settles this PR and the merge train lands it; until then %s brings in nothing newer.\n' \
    "$(git_ log -1 --format=%b)" "$HOST" "$UPDATE"
}
