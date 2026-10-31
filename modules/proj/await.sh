# What a job awaits on GitHub (proj await): the PR or issue a line names, what
# of it is news, and the pass that gives a job whose awaits changed its next
# turn. Part of proj (proj.sh), on its own for its size.

# OWNER/REPO N from a PR or issue URL, or OWNER/REPO#N.
target() {
  local t=${1#https://github.com/}
  if [[ $t =~ ^([^/#]+/[^/#]+)/(pull|issues)/([0-9]+) ]]; then echo "${BASH_REMATCH[1]} ${BASH_REMATCH[3]}"
  elif [[ $t =~ ^([^/#]+/[^/#]+)#([0-9]+)$ ]]; then echo "${BASH_REMATCH[1]} ${BASH_REMATCH[2]}"
  else return 1
  fi
}

# What a job waiting on a PR or issue would want to hear about, as one line of
# JSON: a change in it is news. Checks count only once they've all finished,
# so a run of them wakes the job once. REST, not GraphQL, whose budget is
# shared with everything else under the login.
fingerprint() { # repo n
  local issue comments pr sha reviews status checks
  issue=$("$GH" api "repos/$1/issues/$2") || return
  # Comments on the issue or PR, but not proj-inbox's own (MARK): a job
  # awaiting its own issue would wake on every answer the inbox posts there.
  comments=$("$GH" api --paginate --slurp "repos/$1/issues/$2/comments?per_page=100" |
    jq --arg mark "$MARK" '[add // [] | .[] | select(.body | contains($mark) | not)] | length') || return
  if ! jq -e 'has("pull_request")' <<<"$issue" >/dev/null; then
    jq -c --argjson comments "$comments" '{state, comments: $comments}' <<<"$issue"
    return
  fi
  # Each answer is cut down to what's compared before it's passed on: a whole
  # one (a long PR body, hundreds of reviews) can be past what one argument
  # may hold.
  pr=$("$GH" api "repos/$1/pulls/$2" | jq -c '{state, merged, head: .head.sha, comments, review_comments}') || return
  sha=$(jq -r .head <<<"$pr")
  # Every page: a review or check past the first hundred is news too. Reviews
  # are counted by state, so a dismissed one is news as well as a new one.
  reviews=$("$GH" api --paginate --slurp "repos/$1/pulls/$2/reviews?per_page=100" |
    jq -c 'add // [] | group_by(.state) | map({(.[0].state): length}) | add // {}') || return
  status=$("$GH" api "repos/$1/commits/$sha/status" | jq -c '{total_count, state}') || return
  checks=$("$GH" api --paginate --slurp "repos/$1/commits/$sha/check-runs?per_page=100" |
    jq -r --argjson status "$status" '
      [.[].check_runs[]] as $runs |
      if any($runs[]; .status != "completed")
         or ($status.total_count > 0 and $status.state == "pending")
      then "running"
      else ([$runs[] | "\(.name)=\(.conclusion)"]
            + (if $status.total_count > 0 then ["status=\($status.state)"] else [] end)
            | sort | join(" "))
      end') || return
  jq -c --argjson comments "$comments" --argjson reviews "$reviews" --arg checks "$checks" \
    '. + {comments: $comments, reviews: $reviews, checks: $checks}' <<<"$pr"
}

# `_watch [OWNER/REPO]`: jobs waiting on GitHub whose PRs or issues (in
# that repo, or any) changed get their next turn. A line of JOBS/ID/await
# is "OWNER/REPO#N <tab> since <tab> fingerprint". One pass at a time,
# whoever started it (burrow-events, from before a restart too, or a
# hand): two would each write WAKE from what they saw.
watch_awaits() {
  local only=${1:-} retry='' pass f id turn waits news lines t since was repo n now expired lk
  exec {pass}>>"$WAKE.lock"
  flock "$pass"
  for f in "$jobs"/*/await; do
    [ -s "$f" ] || continue
    id=$(basename "$(dirname "$f")")
    case $(state "$id") in queued | running) continue ;; esac
    turn=$(cat "$jobs/$id/turns" 2>/dev/null) || continue
    # Gone already: another _watch (burrow-events', or a hand-run one) has
    # started its next turn.
    waits=$(cat "$f" 2>/dev/null) || continue
    news='' lines=''
    while IFS=$'\t' read -r t since was; do
      [ -n "$t" ] || continue
      if ! read -r repo n < <(target "$t") || ! [[ $since =~ ^[0-9]+$ ]] ||
        { [ -n "$was" ] && ! jq -e 'type == "object"' <<<"$was" >/dev/null 2>&1; }; then
        # Not a line proj await writes (one edited by hand): the job hears
        # so rather than waiting on it for good.
        news+="- $t: not something proj can watch; no longer watched"$'\n'
        continue
      fi
      # GitHub's names are case-blind: the await's spelling needn't be the event's.
      if [ -n "$only" ] && [ "${repo,,}" != "${only,,}" ]; then
        lines+="$t"$'\t'"$since"$'\t'"$was"$'\n'
        continue
      fi
      # GitHub can't be asked just now: looked at again in five minutes.
      now=$(fingerprint "$repo" "$n" </dev/null) || { now=''; retry+="$repo"$'\t'$(($(date +%s) + 300))$'\n'; }
      expired=$(($(date +%s) - since >= AWAIT_HOURS * 3600))
      if [ -n "$now" ] && [ -n "$was" ] && [ "$now" != "$was" ]; then
        news+="- $t: $(jq -rn --argjson was "$was" --argjson now "$now" '
          [$now | keys[] as $k | select($was[$k] != $now[$k]) | "\($k) \($was[$k] | tojson) → \($now[$k] | tojson)"] | join(", ")')"$'\n'
      elif [ "$expired" = 1 ] && [ -n "$now" ]; then
        news+="- $t: nothing changed in ${AWAIT_HOURS}h; no longer watched"$'\n'
      elif [ "$expired" = 1 ]; then
        # The deadline holds even when GitHub can't be read (down, or the
        # repo gone): the job hears that rather than waiting for good.
        news+="- $t: couldn't be read from GitHub, and ${AWAIT_HOURS}h have passed; no longer watched"$'\n'
      else
        lines+="$t"$'\t'"$since"$'\t'"${was:-$now}"$'\n'
      fi
    done <<<"$waits"
    # GitHub was asked without the lock, so a reply may have started a
    # turn meanwhile (that turn owns the job's waits), or proj stop ended
    # the job: either way these are dropped.
    exec {lk}>>"$jobs/$id/lock"
    flock "$lk"
    if [ "$(cat "$jobs/$id/turns")" != "$turn" ] || [ "$(cat "$f" 2>/dev/null)" != "$waits" ]; then
      :
    elif [ -n "$news" ]; then
      # The new turn clears what's left: if it still waits, it says so again.
      # One that can't start keeps its waits, and is tried again in five
      # minutes.
      start_turn "$id" "News on what you were waiting on:

$news
Carry on from there. Anything you left in the background last turn was stopped when it ended." "" ||
        retry+=$(cut -f1 <<<"$waits" | sed 's/#.*//' | awk -v at=$(($(date +%s) + 300)) '{ print $0 "\t" at }')$'\n'
    else
      printf '%s' "$lines" >"$f.new" && mv -f "$f.new" "$f"
    fi
    exec {lk}>&-
  done
  # When each repo needs a look next, for burrow-events: the deadline of
  # every job's waits that's still to come, and the retries. One already
  # past is a turn that couldn't start (its retry stands for it) or one
  # this pass skipped as running, whose end kicks proj-watch itself. A pass
  # over one repo keeps the other repos' wake-ups (their retries too).
  if [ -n "$WAKE" ]; then
    { [ -z "$only" ] || jq -r --arg only "${only,,}" --argjson now "$(date +%s)" '
        to_entries[] | select((.key | ascii_downcase) != $only and (.value | type) == "number" and .value > $now)
        | "\(.key)\t\(.value)"' "$WAKE" 2>/dev/null || true
      { cat "$jobs"/*/await 2>/dev/null || true; } | awk -F '\t' -v h="$AWAIT_HOURS" -v now="$(date +%s)" '
        $2 ~ /^[0-9]+$/ && split($1, t, "#") == 2 && $2 + h * 3600 + 1 > now { print t[1] "\t" ($2 + h * 3600 + 1) }'
      printf '%s' "$retry"
    } | jq -Rn 'reduce (inputs | split("\t")) as [$r, $at] ({}; .[$r] = ([.[$r] // empty, ($at | tonumber)] | min))' \
      >"$WAKE.new"
    mv -f "$WAKE.new" "$WAKE"
  fi
}
