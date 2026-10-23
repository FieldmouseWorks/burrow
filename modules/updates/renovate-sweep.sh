# renovate-sweep: Renovate PRs that can't merge themselves get a Claude.
#
# Renovate merges its own PRs once CI is green. The ones it can't (CI red,
# or a major version, which the owner merges) would otherwise sit there.
# Every few hours this hands a repo's stuck PRs to one Claude job in that
# project, which fixes them on their branches so they can merge. Once per PR
# head, so a fix that's pushed gets a fresh look only if it's still stuck.
# The autoland steward's own status (SETTLED) doesn't count as a check. (DIR,
# SRC, OWNER, NOTIFY and SETTLED come from ../updates.nix.)
#
# The job's verdict on a major is a label: $OK when it can merge as it is,
# $HELD when it shouldn't (the job's comment says why). The majors notice
# lists only green $OK ones as ready, and $HELD ones as waiting on a decision;
# a major with neither hasn't been read yet, so it isn't offered. A new head
# drops the old verdict before the next job reads it.
OK="major-ok"
HELD="major-held"

state=$DIR/renovate-sweep
src=$SRC
mkdir -p "$state"
ready=$(mktemp)
held=$(mktemp)
trap 'rm -f "$ready" "$held"' EXIT

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
      verdict=$(jq -r --arg ok "$OK" --arg held "$HELD" '[.labels[]?.name] |
        if index($held) != null then "held" elif index($ok) != null then "ok" else "" end' <<<"$pr")
      pending=$(jq -r '[.statusCheckRollup[]? | select((.context // .name) != $settled) | (.conclusion // .state // "" | ascii_upcase)
        | select(. == "" or . == "PENDING" or . == "EXPECTED")] | length' --arg settled "$SETTLED" <<<"$pr")
      # No checks at all isn't "passed CI": nothing has tested it yet.
      checks=$(jq -r '[.statusCheckRollup[]? | select((.context // .name) != $settled)] | length' --arg settled "$SETTLED" <<<"$pr")
      key="$state/${repo//\//_}-$n"
      seen=$(cat "$key" 2>/dev/null || true)
      # A major's key also says the job was asked for a verdict, so a major
      # handed over before verdicts existed is handed over once more.
      want=$sha
      [ "$major" != true ] || want="$sha verdict"
      # A verdict is about the head the job read. Until this head has been
      # handed to a job, a label on it is left over from an older one.
      [ "$seen" = "$want" ] || verdict=""
      if [ "$major" = true ] && [ "$verdict" = held ]; then
        echo "- $title: https://github.com/$repo/pull/$n" >>"$held"
      elif [ "$major" = true ] && [ "$verdict" = ok ] && [ "$checks" -gt 0 ] && [ "$red" -eq 0 ] && [ "$pending" -eq 0 ]; then
        echo "- $title: https://github.com/$repo/pull/$n" >>"$ready"
      fi
      [ "$red" -gt 0 ] || [ "$major" = true ] || continue
      [ "$seen" != "$want" ] || continue
      if [ "$major" = true ]; then
        # The job labels what it read; the labels have to exist first.
        [ -n "${labelled:-}" ] || gh label create "$OK" -R "$repo" --force --color 0e8a16 \
          --description "Major update a Claude job read: fine to merge as it is" >/dev/null 2>&1 || true
        [ -n "${labelled:-}" ] || gh label create "$HELD" -R "$repo" --force --color d93f0b \
          --description "Major update a Claude job read: needs a decision first (see its comment)" >/dev/null 2>&1 || true
        labelled=1
        gh pr edit "$n" -R "$repo" --remove-label "$OK" --remove-label "$HELD" >/dev/null 2>&1 || true
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
Make them mergeable. Look for a cause they share first (often the same failure on several branches), and check whether the default branch itself is red: if it is, fix that in its own PR and say so on each Renovate PR rather than patching every branch. Then, per PR: check out the branch in your own worktree (git worktree add), fix whatever the update breaks (code, config, lockfile), run the tests, and push to the same branch. Renovate merges minor and patch PRs itself once CI is green. A major one waits for $OWNER: leave a short comment on the PR with the breaking changes that touch this project and what you changed for them, and don't merge it. Then give every major PR exactly one of two labels, once its CI is green or you've given up on it: $OK if $OWNER can merge it as it is, or $HELD if it needs a decision or more work first (a support floor to drop, a migration, a regression), with the reason in your comment. $OWNER is told about $OK PRs as one-tap merges, so never put $OK on one you'd argue against merging. If one can't or shouldn't be done (an upstream regression, a migration that needs a decision), comment on the PR saying why, in one or two lines, and close it only if it's a dead end." >/dev/null &&
      echo "$repo: $(wc -l <"$todo") PRs handed to one Claude job"
  fi
  rm -f "$todo"
done

# Major updates are the owner's to merge: one notice listing them all, the
# ones a job cleared apart from the ones it held.
if [ -s "$ready" ] || [ -s "$held" ]; then
  body=""
  if [ -s "$ready" ]; then
    body="These passed CI, and a Claude job read their breaking changes and found nothing in the way (its comment on each PR says what changed). Merging is one tap each:
$(cat "$ready")"
  fi
  if [ -s "$held" ]; then
    [ -z "$body" ] || body="$body

"
    body="${body}These need a decision before they merge. A Claude job read them and held them; its comment on each PR says why:
$(cat "$held")"
  fi
  "$NOTIFY" open majors "Major updates waiting on you" "$body"
else
  "$NOTIFY" close majors "No major updates waiting."
fi
