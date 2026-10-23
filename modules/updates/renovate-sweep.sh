# renovate-sweep: Renovate PRs that can't merge themselves get a Claude.
#
# Renovate merges its own PRs once CI is green. The ones it can't (CI red,
# or a major version, which the owner merges) would otherwise sit there.
# Every few hours this hands a repo's stuck PRs to one Claude job in that
# project, which fixes them on their branches so they can merge. Once per PR
# head, so a fix that's pushed gets a fresh look only if it's still stuck.
# The autoland steward's own status (SETTLED) doesn't count as a check. (DIR,
# SRC, OWNER, NOTIFY and SETTLED come from ../updates.nix.)

state=$DIR/renovate-sweep
src=$SRC
mkdir -p "$state"
ready=$(mktemp)
trap 'rm -f "$ready"' EXIT

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
      pending=$(jq -r '[.statusCheckRollup[]? | select((.context // .name) != $settled) | (.conclusion // .state // "" | ascii_upcase)
        | select(. == "" or . == "PENDING" or . == "EXPECTED")] | length' --arg settled "$SETTLED" <<<"$pr")
      if [ "$major" = true ] && [ "$red" -eq 0 ] && [ "$pending" -eq 0 ]; then
        echo "- $title: https://github.com/$repo/pull/$n" >>"$ready"
      fi
      [ "$red" -gt 0 ] || [ "$major" = true ] || continue
      key="$state/${repo//\//_}-$n"
      [ "$(cat "$key" 2>/dev/null)" != "$sha" ] || continue
      echo "$sha" >"$key"
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
Make them mergeable. Look for a cause they share first (often the same failure on several branches), and check whether the default branch itself is red: if it is, fix that in its own PR and say so on each Renovate PR rather than patching every branch. Then, per PR: check out the branch in your own worktree (git worktree add), fix whatever the update breaks (code, config, lockfile), run the tests, and push to the same branch. Renovate merges minor and patch PRs itself once CI is green. A major one waits for $OWNER: leave a short comment on the PR with the breaking changes that touch this project and what you changed for them, and don't merge it. If one can't or shouldn't be done (an upstream regression, a migration that needs a decision), comment on the PR saying why, in one or two lines, and close it only if it's a dead end." >/dev/null &&
      echo "$repo: $(wc -l <"$todo") PRs handed to one Claude job"
  fi
  rm -f "$todo"
done

# Green major updates are the owner's to merge: one notice listing them all.
if [ -s "$ready" ]; then
  "$NOTIFY" open majors "Major updates ready to merge" "These passed CI, and a Claude job has read their breaking changes (see its comment on each PR). Merging is one tap each:
$(cat "$ready")"
else
  "$NOTIFY" close majors "No major updates waiting."
fi
