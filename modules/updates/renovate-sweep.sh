# renovate-sweep: Renovate PRs that can't merge themselves get a Claude.
#
# Renovate merges its own PRs once CI is green. The ones it can't (CI red,
# or a major version, which needs a verdict first) would otherwise sit there.
# Every few hours this hands a repo's stuck PRs to one Claude job in that
# project, which fixes them on their branches so they can merge. Once per PR
# head, so a fix that's pushed gets a fresh look only if it's still stuck.
# The autoland steward's own status (SETTLED) doesn't count as a check. (DIR,
# SRC, OWNER, NOTIFY and SETTLED come from ../updates.nix.)
#
# The job's verdict on a major: ok when it can merge as it is, held when it
# shouldn't (its comment says why). The job ends that comment with a marker
# naming the head it cleared, `<!-- major-verdict: ok|held SHA -->`, and
# labels the PR $OK or $HELD for people to see. A verdict counts only for the
# head it names and only from our own login, so a job still finishing on an
# older head, a push since, or someone else's comment can't clear a PR. The
# autoland steward merges an ok one once its CI is green (the same rule);
# held ones are listed for the owner here, as waiting on a decision.
OK="major-ok"
HELD="major-held"
me=$(gh api user --jq .login 2>/dev/null || true)

state=$DIR/renovate-sweep
src=$SRC
mkdir -p "$state"
held=$(mktemp)
trap 'rm -f "$held"' EXIT

for d in "$src"/*/; do
  p=$(basename "$d")
  url=$(git -C "$d" remote get-url origin 2>/dev/null) || continue
  repo=$(sed -E 's#^(https://github.com/|git@github.com:)##; s#\.git$##' <<<"$url")
  [[ $repo == */* ]] || continue

  # Every stuck PR of the repo goes to one job: they usually share a cause
  # (a toolchain bump, a lint the new versions trip), and one job per PR
  # would fix it several times over, side by side.
  todo=$(mktemp)
  gh pr list -R "$repo" --state open --search "head:renovate/" \
    --json number,title,headRefName,headRefOid,labels,statusCheckRollup 2>/dev/null |
    jq -c '.[]' |
    while read -r pr; do
      n=$(jq -r .number <<<"$pr")
      sha=$(jq -r .headRefOid <<<"$pr")
      title=$(jq -r .title <<<"$pr")
      branch=$(jq -r .headRefName <<<"$pr")
      # Check runs carry a conclusion (empty while running), commit statuses a state.
      red=$(jq -r '[.statusCheckRollup[]? | select((.context // .name) != $settled) | (.conclusion // .state // "" | ascii_upcase)
        | select(. == "FAILURE" or . == "ERROR" or . == "TIMED_OUT" or . == "STARTUP_FAILURE")] | length' --arg settled "$SETTLED" <<<"$pr")
      major=$(jq -r '[.labels[]?.name] | index("major") != null' <<<"$pr")
      key="$state/${repo//\//_}-$n"
      seen=$(cat "$key" 2>/dev/null || true)
      # A major's key also says the job was asked for a verdict, so a major
      # handed over before verdicts existed is handed over once more.
      want=$sha
      [ "$major" != true ] || want="$sha verdict"
      verdict=""
      if [ "$major" = true ] && [ -n "$me" ]; then
        verdict=$(gh pr view "$n" -R "$repo" --json comments 2>/dev/null | jq -r --arg me "$me" --arg sha "$sha" '
          [.comments[] | select(.author.login == $me) | .body
            | capture("<!-- major-verdict: (?<v>ok|held) (?<sha>[0-9a-f]{40}) -->") | select(.sha == $sha)] | last
          | if . != null then .v else "" end' || true)
      fi
      if [ "$major" = true ] && [ "$verdict" = held ]; then
        echo "- $title: https://github.com/$repo/pull/$n" >>"$held"
      fi
      [ "$red" -gt 0 ] || [ "$major" = true ] || continue
      [ "$seen" != "$want" ] || continue
      # A job that pushed its fix and then cleared the new head is done:
      # nothing for another job to do there.
      if [ -n "$verdict" ] && [ "$red" -eq 0 ]; then
        echo "$want" >"$key"
        continue
      fi
      if [ "$major" = true ]; then
        # The job labels what it read; the labels have to exist first.
        [ -n "${labelled:-}" ] || gh label create "$OK" -R "$repo" --force --color 0e8a16 \
          --description "Major update a Claude job read: fine to merge as it is" >/dev/null 2>&1 || true
        [ -n "${labelled:-}" ] || gh label create "$HELD" -R "$repo" --force --color d93f0b \
          --description "Major update a Claude job read: needs a decision first (see its comment)" >/dev/null 2>&1 || true
        labelled=1
        # The old head's labels would mislead a reader; the marker is what counts.
        if jq -e --arg ok "$OK" --arg held "$HELD" '[.labels[]?.name] | index($ok) != null or index($held) != null' <<<"$pr" >/dev/null; then
          gh pr edit "$n" -R "$repo" --remove-label "$OK" --remove-label "$HELD" >/dev/null 2>&1 || true
        fi
      fi
      echo "$want" >"$key"
      if [ "$major" = true ]; then
        why="a major version bump"
        [ "$red" -eq 0 ] || why="$why, and its CI fails"
      else
        why="its CI fails"
      fi
      echo "- #$n \"$title\" (branch $branch): $why" >>"$todo"
    done || true
  if [ -s "$todo" ]; then
    proj run "$p" "Renovate's PRs in $repo can't merge themselves:
$(cat "$todo")
Make them mergeable. Look for a cause they share first (often the same failure on several branches), and check whether the default branch itself is red: if it is, fix that in its own PR and say so on each Renovate PR rather than patching every branch. Then, per PR: check out the branch in your own worktree (git worktree add), fix whatever the update breaks (code, config, lockfile), run the tests, and push to the same branch. Settle every open review thread on it as well: fix it, or reply with why not, then resolve it (gh api graphql, resolveReviewThread); the autoland steward leaves a red Renovate PR's threads to you. Renovate merges minor and patch PRs itself once CI is green. Don't merge a major one yourself: leave a short comment on the PR with the breaking changes that touch this project and what you changed for them. Then give every major PR a verdict, once its CI is green or you've given up on it: ok if it can merge as it is, or held if it needs a decision from $OWNER or more work first (a support floor to drop, a migration, a regression), with the reason in your comment. End that comment with the marker <!-- major-verdict: ok SHA --> or <!-- major-verdict: held SHA -->, where SHA is the full head commit of the PR after your last push (gh pr view N --json headRefOid), and label the PR $OK or $HELD to match. The autoland steward merges an ok one on that marker once its CI is green, so never mark one ok that you'd argue against merging. If one can't or shouldn't be done (an upstream regression, a migration that needs a decision), comment on the PR saying why, in one or two lines, and close it only if it's a dead end." >/dev/null &&
      echo "$repo: $(wc -l <"$todo") PRs handed to one Claude job"
  fi
  rm -f "$todo"
done

# The held majors wait on the owner's decision: one notice listing them all.
# The ok ones aren't theirs: the steward merges those.
if [ -s "$held" ]; then
  "$NOTIFY" open majors "Major updates waiting on you" "A Claude job read these and held them; its comment on each PR says why. They need a decision before they merge:
$(cat "$held")"
else
  "$NOTIFY" close majors "No major updates waiting."
fi
