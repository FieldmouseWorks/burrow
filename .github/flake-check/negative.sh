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
negative() {
  work=$(mktemp -d "$RUNNER_TEMP/negative.XXXXXX")
  trap 'rm -rf -- "$work"' EXIT
  : > "$work/tests"
  : > "$work/checks"
  : > "$work/skipped"
  local tip fork f meta mode oid status tree commit when side name q b n system at tree_at
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
  # by side as there are cores while each has 2G of the memory available:
  # its .drv path, or nothing where it doesn't evaluate.
  names() { nix eval --no-update-lock-file --option eval-cache false --json "$1#." --apply "o: builtins.attrNames (o.checks.$system or { })"; }
  names "$work/base" > "$work/base.names" || { echo "the base's checks don't evaluate"; return 1; }
  if ! names "$work/negative" > "$work/negative.names"; then
    echo "the checks don't evaluate with the new tests on the old code: they fail there"
    found fail "$tree"
    return 0
  fi
  jq -rs 'add | unique | .[]' "$work/base.names" "$work/negative.names" > "$work/names"
  side=$(nproc)
  room=$(( $(awk '$1 == "MemAvailable:" { print $2 }' /proc/meminfo) * 1024 / 2147483648 ))
  [ "$room" -ge "$side" ] || side=$room
  [ "$side" -ge 1 ] || side=1
  # A check's derivation, by its name as a Nix string, in the tree at $1:
  # its .drv path in $3, made a GC root at once, until the build is done;
  # nothing in $3 where it doesn't evaluate, and why in $3.err. One that
  # isn't a valid derivation (or that a GC took in between) is an error.
  evaluate() {
    local drv
    drv=$(nix eval --no-update-lock-file --option eval-cache false --raw "$1#." \
      --apply "o: o.checks.$system.$2.drvPath" 2> "$3.err") || { : > "$3"; return 0; }
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
  done < "$work/names"
  wait || true
  # A check that reads none of the PR's test files has the base's
  # derivation, which passed there. The rest are built, but those SKIP
  # names; one that doesn't evaluate on the negative tree fails there.
  local targets=() caught=0
  for ((k = 1; k <= i; k++)); do
    name=$(cat "$work/drvs/$k.name")
    b=$(cat "$work/drvs/$k.base")
    n=$(cat "$work/drvs/$k.negative")
    if [ -z "$n" ]; then
      echo "check $name doesn't evaluate with the new tests on the old code:"
      cat "$work/drvs/$k.negative.err"
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
      targets+=("$n^*")
    fi
  done
  if [ ${#targets[@]} -gt 0 ] && ! build_checks "" "${targets[@]}"; then
    caught=1
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
