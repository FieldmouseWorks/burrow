# The shared CI's negative run, flake-check.yml's negative step: a PR's new
# tests on its base's code, which have to fail there (Conary's
# docs/ci-attestation.md, "Negative runs", says what this does and why). Its
# env is the step's: BASE (the PR's base branch), HEAD_SHA (its head), SKIP
# (the checks not to build), GITHUB_OUTPUT and RUNNER_TEMP, and PATHS_ONLY.
# It runs in a checkout of the head with the base branch's history. With
# PATHS_ONLY true it only says whether the PR needs a negative (its `needs`
# output), which git alone can tell, so a PR that needs none never waits
# for nix to be installed.
#
# What it found goes in $RUNNER_TEMP/negative.json (the attestation's
# predicate, less its revision and log) and the step's outputs, and what it
# printed in $RUNNER_TEMP/negative.log. It fails when the new tests passed
# on the old code, and when it couldn't tell (then with no negative.json).
#
# GitHub runs a step's `shell: bash` with -eo pipefail.
set -eo pipefail
# How checks are built (build.sh, beside this).
# shellcheck source=build.sh
source "${BASH_SOURCE[0]%/*}/build.sh"
# Test files, as git's glob pathspecs (`*` within a directory, `**/` any
# depth, none included). Here and not an input: the caller's workflow comes
# from the PR, and a PR mustn't choose its own judge.
tests_specs=(':(glob)tests/**' ':(glob)**/tests.rs' ':(glob)**/tests/**' ':(glob)**/test_*.py'
  ':(glob)**/*_test.*' ':(glob)**/*.test.ts' ':(glob)src/test/**')
# What it found, as negative.json and the step's outputs: the result, the
# tree it built on (none for none), the PR's test files, the checks it
# built and those it would have but SKIP names.
found() {
  local result=$1 tree=$2
  jq -n --arg result "$result" --arg tree "$tree" \
    --rawfile tests "$work/tests" --rawfile checks "$work/checks" --rawfile skipped "$work/skipped" \
    'def list: split("\u0000") | map(select(. != ""));
      {$result, tree: (if $tree == "" then null else $tree end),
        tests: ($tests | list), checks: ($checks | list), skipped: ($skipped | list)}' \
    > "$RUNNER_TEMP/negative.json"
  echo "result=$result" >> "$GITHUB_OUTPUT"
}
# nix's evaluations here log as its JSON (--log-format internal-json), so
# what each did on the way can be read. What the one logged in $1 said.
errors() {
  jq -rR 'if startswith("@nix ") then .[5:] | fromjson? | select(.action == "msg" and .level <= 1) | .msg else . end' "$1"
}
# Whether an evaluation that ended with $1, having logged $2, failed on
# what it evaluated: nix's own failure (1, not a signal or a limit's
# kill), with nothing built, fetched or substituted on the way, any of
# which could be what failed (a store path it only had to find is
# fine), and an error nix places in code, at a file's line and column,
# as the evaluator's are. One it doesn't place (the daemon or the store
# out of reach, nix out of memory, anything else of nix's own) says
# nothing. Nix doesn't say which errors are the evaluator's own, only
# where they were, so a store failure it places inside the expression
# (one a builtin raises, under its caller's frames) reads as one too:
# failed_there's second try, apart from the store, is what stops that.
# With $3 `first`, what it built or fetched on the way isn't held against
# it: that's for a first try, which can be the one that fetches an input
# (any, on a fresh runner), and whose second try is held to all of it.
# Says why not when it isn't.
own() {
  local why
  if [ "$1" != 1 ]; then
    echo "it ended with $1, not an evaluation's failure, so it can't tell"
    return 1
  fi
  if ! why=$(jq -nrR --arg first "${3:-}" '[inputs | select(startswith("@nix ")) | .[5:] | fromjson?] as $log
      | first(([$log[] | select($first != "first" and .action == "start" and (.type | IN(100, 101, 105, 108, 109, 110, 111, 112))) | .text]
          | select(length > 0) | "it built or fetched on the way (\(join("; ")))"),
        ([$log[] | select(.action == "msg" and .level == 0)
            | select(.file != null or (.msg | gsub("\u001b\\[[0-9;]*m"; "") | test("\\bat [^\n]+:[0-9]+:[0-9]+")))]
          | select(length == 0) | "nix doesn'"'"'t place its error in any code"))' "$2"); then
    echo "its log can't be read, so it can't tell"
    return 1
  fi
  if [ -n "$why" ]; then
    echo "$why, so it can't tell"
    return 1
  fi
}
# Whether an evaluation on the negative tree failed on what it
# evaluated, where $1 and $2 are its exit status and log, and $3 how
# it's run, as `$3 TREE ARGS...` with the trees under $4 and $5.. its
# further arguments: its own failure (own, as a first try), and again,
# held to all of own, on a second try
# apart from the daemon, the machine's store and the network, in a
# store of the job's own that nothing else uses and no substituter
# fills (apart, which $3 puts before its arguments), with the base's
# evaluation, which worked beside the first, working there after it.
# A failure of the store, the daemon or the network the first time
# can't happen the second, and the base's working in the same store
# says that store works.
failed_there() {
  local rc=$1 log=$2 how=$3 trees=$4
  shift 4
  own "$rc" "$log" first || return 1
  local apart=(--store "local?root=$work/apart" --option substituters "")
  rc=0
  "$how" "$trees/negative" "$@" > /dev/null 2> "$log.again" || rc=$?
  if [ "$rc" = 0 ]; then
    echo "it evaluated on a second try, so it can't tell"
    return 1
  fi
  own "$rc" "$log.again" || return 1
  if ! "$how" "$trees/base" "$@" > /dev/null 2>&1; then
    echo "the base no longer evaluates either, so it can't tell"
    return 1
  fi
}
# Whether every output of the derivation $1 is in the store.
built() {
  local outs
  outs=$(nix-store -q --outputs "$1") || return 1
  [ -n "$outs" ] && xargs -d '\n' nix-store --check-validity <<< "$outs" 2> /dev/null
}
# Whether nix's log of a failed build, $2, says the builder of $1, the
# derivation, failed with an exit code a test gives: in nix's words
# since 2.30 or so, `Cannot build 'DRV'.` and on the next line `Reason:
# builder failed with exit code N.`, and before, `builder for 'DRV'
# failed with exit code N`. A signal, a timeout, a dependency or nix's
# own failure is none of those, and nor is a code a shell gives for
# them: 124, timeout's, and 128 and up, a signal's.
builder_failed() {
  awk -v d="$1" '
    function code(s) { sub(/.*failed with exit code /, "", s); s += 0; return s >= 1 && s < 128 && s != 124 }
    index($0, "builder for \047" d "\047 failed with exit code ") && code($0) { found = 1 }
    named && $0 ~ /^ *Reason: builder failed with exit code / && code($0) { found = 1 }
    { named = index($0, "Cannot build \047" d "\047.") > 0 }
    END { exit !found }' "$2"
}
# Whether the derivation $1 is a fetch: an output with a fixed hash,
# which its builder downloads, so its failing can be the network's.
# 0 says it is and 1 that it isn't, as nix's JSON (either version) has
# it; anything else, that it can't be told.
fetch() {
  local json
  json=$(nix derivation show "$1") || return 2
  jq -e '(.derivations // .) | to_entries | if length == 1 then .[0].value.outputs else error("not one derivation") end
    | if type == "object" and length > 0 and all(.[]; type == "object") then any(.[]; has("hash")) else error("no outputs") end' \
    <<< "$json" > /dev/null 2>&1 || { local rc=$?; [ "$rc" = 1 ] && return 1; return 2; }
}
# A derivation's inputs, each with the outputs it takes, one installable a
# line: nix's JSON names them by store path or, in its version 4, by name
# in the store, under `inputDrvs` or, in 4, `inputs.drvs`, and their outputs
# as a list or, since, under `outputs`.
inputs() {
  nix derivation show "$1" | jq -r --arg store "${1%/*}" '(.derivations // .) | to_entries[0].value
    | (.inputs.drvs // .inputDrvs // error("no inputs"))
    | to_entries[] | (.value | if type == "array" then . else .outputs end) as $o
    | select($o | length > 0)
    | "\(if .key | contains("/") then .key else $store + "/" + .key end)^\($o | join(","))"'
}
negative() {
  work=$(mktemp -d "$RUNNER_TEMP/negative.XXXXXX")
  # The store failed_there evaluates apart in is nix's, read-only.
  trap 'chmod -R u+w -- "$work" 2> /dev/null; rm -rf -- "$work"' EXIT
  : > "$work/tests"
  : > "$work/checks"
  : > "$work/skipped"
  local tip fork f meta mode oid status tree commit when side name q b n system at tree_at rc
  tip=$(git rev-parse --verify "refs/remotes/origin/$BASE^{commit}")
  if ! fork=$(git merge-base "$tip" "$HEAD_SHA"); then
    echo "$HEAD_SHA shares no history with $BASE, so it has no base code to fail on"
    return 1
  fi
  echo "fork=$fork" >> "$GITHUB_OUTPUT"
  # The PR's own changes, from where it left its base to its head: a
  # rename as the path it left and the one it made.
  git diff --name-only --no-renames -z "$fork" "$HEAD_SHA" > "$work/changed"
  git diff --name-only --no-renames -z "$fork" "$HEAD_SHA" -- "${tests_specs[@]}" > "$work/tests"
  local added others
  added=$(git diff --name-only --no-renames --diff-filter=d -z "$fork" "$HEAD_SHA" -- "${tests_specs[@]}" | tr '\0' '\n')
  others=$(comm -z -23 <(sort -z "$work/changed") <(sort -z "$work/tests") | tr '\0' '\n')
  if [ -z "$added" ] || [ -z "$others" ]; then
    if [ -z "$added" ]; then
      echo "$HEAD_SHA adds or changes no test file since $fork: no negative needed"
    else
      echo "$HEAD_SHA changes only test files since $fork, so the base's code is its own: no negative needed"
    fi
    found none ""
    echo "needs=false" >> "$GITHUB_OUTPUT"
    return 0
  fi
  echo "the tests $HEAD_SHA adds or changes since $fork:"
  echo "$added"
  echo "needs=true" >> "$GITHUB_OUTPUT"
  [ "$PATHS_ONLY" != true ] || return 0
  # The negative tree: the fork's, with the head's version of each test
  # file the PR changes, gone where the head has none. Made in an index of
  # its own, so the checkout is never touched.
  export GIT_INDEX_FILE=$work/index
  git read-tree "$fork"
  # Each as `:MODE MODE OID OID STATUS`, then its path: the head's mode
  # and blob are the second of each.
  git diff --raw --no-abbrev --no-renames -z "$fork" "$HEAD_SHA" -- "${tests_specs[@]}" > "$work/raw"
  while IFS= read -r -d '' meta && IFS= read -r -d '' f; do
    read -r _ mode _ oid status <<< "$meta"
    if [ "$status" = D ]; then
      git update-index --force-remove -- "$f"
    else
      git update-index --add --cacheinfo "$mode,$oid,$f"
    fi
  done < "$work/raw"
  tree=$(git write-tree)
  unset GIT_INDEX_FILE
  # A commit of it, dated as the fork is, so the flake's rev and
  # lastModified are the same however often it's made.
  when=$(git show -s --format=%ct "$fork")
  commit=$(GIT_AUTHOR_DATE="@$when +0000" GIT_COMMITTER_DATE="@$when +0000" \
    git -c user.name=flake-check -c user.email=flake-check@localhost -c commit.gpgsign=false \
    commit-tree "$tree" -p "$fork" -m "the tests of $HEAD_SHA on $fork's code")
  echo "the negative tree: $tree ($commit)"
  # The fork and the negative tree, each checked out on its own, sharing
  # this checkout's objects.
  for at in base:"$fork" negative:"$commit"; do
    git clone -q --shared --no-checkout . "$work/${at%%:*}"
    git -C "$work/${at%%:*}" checkout -q --detach "${at#*:}"
  done
  system=$(nix config show system)
  [[ $system =~ ^[A-Za-z0-9_-]+$ ]] || { echo "can't evaluate for system $system"; return 1; }
  # Each check's derivation in each tree, a nix process each, as many side
  # by side as the job's memory allows (build.sh): its .drv path, or
  # nothing where it doesn't evaluate.
  names() {
    nix eval "${apart[@]}" --no-update-lock-file --option eval-cache false --log-format internal-json \
      --json "$1#." --apply "o: builtins.attrNames (o.checks.$system or { })"
  }
  if ! names "$work/base" > "$work/base.names" 2> "$work/base.names.err"; then
    errors "$work/base.names.err"
    echo "the base's checks don't evaluate, so there's nothing to compare with"
    return 1
  fi
  rc=0
  names "$work/negative" > "$work/negative.names" 2> "$work/negative.names.err" || rc=$?
  if [ "$rc" != 0 ]; then
    errors "$work/negative.names.err"
    if ! failed_there "$rc" "$work/negative.names.err" names "$work"; then
      return 1
    fi
    echo "the checks don't evaluate with the new tests on the old code: they fail there"
    found fail "$tree"
    return 0
  fi
  jq -rs 'add | unique | .[]' "$work/base.names" "$work/negative.names" > "$work/names"
  side_by_side
  # A check's derivation, by its name as a Nix string, in the tree at $1:
  # its .drv path in $3, made a GC root at once, until the build is done;
  # nothing in $3 where it doesn't evaluate, and nix's log in $3.err and
  # exit status in $3.rc. One that isn't a valid derivation (or that a GC
  # took in between) is an error.
  drv_of() {
    nix eval "${apart[@]}" --no-update-lock-file --option eval-cache false --log-format internal-json \
      --raw "$1#." --apply "o: o.checks.$system.$2.drvPath"
  }
  evaluate() {
    local drv rc=0
    drv=$(drv_of "$1" "$2" 2> "$3.err") || rc=$?
    echo "$rc" > "$3.rc"
    if [ "$rc" != 0 ]; then
      : > "$3"
      return 0
    fi
    # shellcheck disable=SC2016 # Nix's own strings
    nix-instantiate --add-root "$roots/${3##*/}" --argstr drv "$drv" \
      --expr '{ drv }: { type = "derivation"; name = "root"; outputName = "out"; drvPath = drv; }' > /dev/null
    echo "$drv" > "$3"
  }
  mkdir "$work/drvs"
  roots=$work/roots
  mkdir "$roots"
  local i=0
  while IFS= read -r name; do
    i=$((i + 1))
    # The name as a Nix string: JSON's, with an interpolation's $ escaped.
    q=$(jq -rn --arg n "$name" '$n | @json | gsub("\\$"; "\\$")')
    for tree_at in base negative; do
      while [ "$(jobs -pr | wc -l)" -ge "$side" ]; do wait -n || true; done
      evaluate "$work/$tree_at" "$q" "$work/drvs/$i.$tree_at" &
    done
    echo "$name" > "$work/drvs/$i.name"
    echo "$q" > "$work/drvs/$i.q"
  done < "$work/names"
  wait || true
  # A check that reads none of the PR's test files has the base's
  # derivation, which passed there. The rest are built, but those SKIP
  # names. One the base has that doesn't evaluate there leaves nothing to
  # compare with; one that doesn't evaluate on the negative tree, while it
  # does on the base, fails there.
  local targets=() bases=() caught=0
  for ((k = 1; k <= i; k++)); do
    name=$(cat "$work/drvs/$k.name")
    q=$(cat "$work/drvs/$k.q")
    b=$(cat "$work/drvs/$k.base")
    n=$(cat "$work/drvs/$k.negative")
    if [ -z "$b" ] && jq -e --arg n "$name" 'index([$n])' "$work/base.names" > /dev/null; then
      echo "check $name doesn't evaluate on the base, so there's nothing to compare with:"
      errors "$work/drvs/$k.base.err"
      return 1
    elif [ -z "$n" ]; then
      echo "check $name doesn't evaluate with the new tests on the old code:"
      errors "$work/drvs/$k.negative.err"
      failed_there "$(cat "$work/drvs/$k.negative.rc")" "$work/drvs/$k.negative.err" drv_of "$work" "$q" || return 1
      printf '%s\0' "$name" >> "$work/checks"
      caught=1
    elif [ "$n" = "$b" ]; then
      :
    elif [[ " $SKIP " == *" $name "* ]]; then
      echo "check $name reads the new tests, but isn't built here (build-skip)"
      printf '%s\0' "$name" >> "$work/skipped"
    else
      echo "check $name reads the new tests: building it"
      printf '%s\0' "$name" >> "$work/checks"
      targets+=("$n")
      bases+=("$b")
    fi
  done
  # A check failed on the old code only when, with everything it's
  # built from built and the base's same check too, it failed again on
  # its own, and nix says its own builder failed, with an exit code: on
  # what the new tests change. One that builds then passed. Anything else
  # (a fetch, the daemon, a dependency, a signal or a timeout, a check
  # broken on the base too) can't say. Each build goes through
  # build_checks, so one with a VM to run waits for its slot.
  if [ ${#targets[@]} -gt 0 ] && ! build_checks "" "${targets[@]/%/^*}"; then
    local unsure=0 deps kind
    for k in "${!targets[@]}"; do
      if built "${targets[$k]}"; then
        continue
      fi
      echo "${targets[$k]} failed: checking that it's its own failure"
      if ! inputs "${targets[$k]}" > "$work/inputs"; then
        echo "can't tell what ${targets[$k]} is built from"
        unsure=1
        continue
      fi
      mapfile -t deps < "$work/inputs"
      if [ ${#deps[@]} -gt 0 ] && ! build_checks "" "${deps[@]}"; then
        echo "what ${targets[$k]} is built from didn't build, so it can't tell"
        unsure=1
      elif [ -n "${bases[$k]}" ] && ! build_checks "" "${bases[$k]}^*"; then
        echo "${bases[$k]}, the same check on the base, fails too, so it can't tell"
        unsure=1
      elif build_checks "" "${targets[$k]}^*" 2>&1 | tee "$work/again"; then
        echo "${targets[$k]} built once what it's built from had: it passes"
      elif kind=0 && { fetch "${targets[$k]}" || kind=$?; } && [ "$kind" != 1 ]; then
        if [ "$kind" = 0 ]; then
          echo "${targets[$k]} is a fetch, whose failure can be the network's, so it can't tell"
        else
          echo "can't tell whether ${targets[$k]} is a fetch, so it can't tell"
        fi
        unsure=1
      elif builder_failed "${targets[$k]}" "$work/again"; then
        caught=1
      else
        echo "nix doesn't say ${targets[$k]}'s own builder failed with a test's exit code, so it can't tell"
        unsure=1
      fi
    done
    if [ "$caught" = 0 ] && [ "$unsure" = 1 ]; then
      return 1
    fi
  fi
  if [ "$caught" = 1 ]; then
    echo "the new tests fail on the old code, as they should"
    found fail "$tree"
  elif [ -s "$work/skipped" ]; then
    echo "every check built here passed with the new tests on the old code, and those that weren't built (build-skip) are where they'd fail, if anywhere: the caller's runner that builds them has to say"
    found skipped "$tree"
  elif [ ${#targets[@]} -eq 0 ]; then
    echo "::error title=No check runs the new tests::no check of the flake reads the test files $HEAD_SHA changes, so nothing tests them"
    found pass "$tree"
    return 1
  else
    echo "::error title=The new tests pass on the old code::with $HEAD_SHA's test files on $fork's code every check that reads them passes, so they don't test what it changes. Make a test fail without the change, or land a tidy of the tests on its own."
    found pass "$tree"
    return 1
  fi
}
negative 2>&1 | tee "$RUNNER_TEMP/negative.log"
