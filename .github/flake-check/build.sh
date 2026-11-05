# Building checks, for the shared CI's gate (gate.sh) and its negative run
# (negative.sh), which source this: on a machine with heavy's slots, a check
# with a VM test to run builds only while it holds one. The caller sets
# roots, a directory the VM tests' outputs are rooted in while it runs.
#
# GitHub runs a step's `shell: bash` with -eo pipefail, and so do they.
# On a machine with heavy's slots (burrow.cpu: self-hosted, never a
# hosted runner), a VM test builds only while it holds one: beside
# the jobs holding them, one ran past the test driver's hour. A
# slot's share of the machine is HEAVY_JOBS threads, so that's as
# many builds as it starts.
has_slots() { command -v heavy > /dev/null; }
in_slot() {
  if has_slots; then
    # shellcheck disable=SC2016 # bash -c's own "$@", not this one's
    heavy -- bash -c 'exec "$@" --max-jobs "$HEAVY_JOBS"' bash "$@"
  else
    "$@"
  fi
}
# Prints which derivations in these ones' closures still have a VM
# to run, by store name: those requiring kvm or nixos-test with an
# output it couldn't root as it stands. A rerun's tests, already
# built, wait for nothing, and stay built under $roots until the
# gate's end; rooting a path builds nothing. Nix's JSON has
# structured attrs as structuredAttrs or, before 2.25 or so, in
# env.__json, and anything else with no env is an error.
unbuilt_vms() {
  local name out outs i=0 pending
  nix derivation show -r "${@/%/^*}" | jq -r '(.derivations // .) | to_entries[]
    | (.value.env | if type != "object" then error("a derivation with no env") else . end) as $env
    | select(($env.requiredSystemFeatures // "" | [splits("[ \t\n\r]+")])
        + (.value.structuredAttrs.requiredSystemFeatures // [])
        + ($env.__json // "{}" | fromjson | .requiredSystemFeatures // [])
      | any(. == "kvm" or . == "nixos-test"))
    | .key | sub(".*/"; "")' > "$RUNNER_TEMP/vms" || return 1
  while read -r name; do
    pending=
    outs=$(nix-store -q --outputs "${1%/*}/$name") || pending=1
    [ -n "$outs" ] || pending=1
    for out in $outs; do
      i=$((i + 1))
      nix-store --add-root "$roots/vm-output-$i" --indirect -r "$out" > /dev/null 2>&1 || pending=1
    done
    [ -z "$pending" ] || echo "$name"
  done < "$RUNNER_TEMP/vms"
}
# Builds these checks (each a derivation and ^*, or the outputs it
# names), keeping the outputs under the link $1 if it's given: first
# those with no VM to run in their closures, with no slot, then the
# rest in one. Both are built by derivation, so neither holds an
# evaluation. A check that can't be told apart counts as a VM test.
build_checks() {
  local link=$1 rc=0 target closure found plain=() vms=() plain_link=(--no-link) vm_link=(--no-link)
  shift
  [ -z "$link" ] || { plain_link=(--out-link "$link"); vm_link=(--out-link "$link-vm"); }
  if ! has_slots; then
    nix build -L "${plain_link[@]}" --keep-going "$@"
    return
  fi
  if ! unbuilt_vms "${@%^*}" > "$RUNNER_TEMP/unbuilt-vms"; then
    echo "couldn't tell which checks are VM tests, so all of them take the slot"
    vms=("$@")
  else
    for target in "$@"; do
      # grep's 1 is no match, and only that's no VM.
      found=2
      if closure=$(nix-store -q -R "${target%^*}"); then
        found=0
        grep -qFf "$RUNNER_TEMP/unbuilt-vms" <<< "$closure" || found=$?
      fi
      if [ "$found" = 1 ]; then plain+=("$target"); else vms+=("$target"); fi
    done
  fi
  if [ ${#plain[@]} -gt 0 ]; then
    nix build -L "${plain_link[@]}" --keep-going "${plain[@]}" || rc=1
  fi
  if [ ${#vms[@]} -gt 0 ]; then
    echo "waiting for a heavy slot for ${#vms[@]} checks with VM tests"
    in_slot nix build -L "${vm_link[@]}" --keep-going "${vms[@]}" || rc=1
  fi
  return $rc
}
# How many evaluations run side by side (in $side): one a core while
# each has 2G of the memory free to this job. Uses $work for scratch.
side_by_side() {
  local free proc kind at top unknown max holds cache limit held inactive left room
  # Free to this job: the machine's available memory, or less where
  # its memory cgroup, or one above that, has a limit (a proj job's
  # or a Claude server's MemoryMax, a container's): what the limit
  # leaves, counting as free the inactive page cache the kernel
  # gives back first. The cgroup is v1's memory controller's when
  # the job has one, cgroup v2's otherwise, read through the mount
  # of its hierarchy that shows the most cgroups above it, however
  # it's mounted (with other v1 controllers, from a subtree). A
  # limit that can't be read (no mount shows the cgroup, or a
  # cgroup with a limit doesn't say what it holds) is unknown, so
  # one evaluation at a time. CGROUP_PROC is where the job's
  # cgroups and mounts are read from (/proc/self; a test's
  # stand-in).
  free=$(( $(awk '$1 == "MemAvailable:" { print $2 }' /proc/meminfo) * 1024 ))
  proc=${CGROUP_PROC:-/proc/self}
  kind='' at='' top='' unknown=''
  # The hierarchy, then the job's cgroup's directory and the
  # mount's, a line each; or the hierarchy and "-" when no mount
  # shows the cgroup. Mount points are unescaped (mountinfo writes
  # a space \040, a tab \011); one that isn't stays unfound.
  # No cgroup file at all is a kernel without cgroups; one that's
  # there but can't be read (or its mounts) is an unknown limit.
  if [ -e "$proc/cgroup" ]; then
    if [ -f "$proc/cgroup" ] && [ -f "$proc/mountinfo" ] && awk '
      function unescape(s) { gsub(/\\040/, " ", s); gsub(/\\011/, "\t", s); return s }
      FILENAME == ARGV[1] {
        controllers = $0; sub(/^[^:]*:/, "", controllers)
        path = controllers; sub(/:.*/, "", controllers); sub(/^[^:]*:/, "", path)
        if (("," controllers ",") ~ /,memory,/) { kind = "v1"; job = path }
        else if (controllers == "" && kind == "") { kind = "v2"; job = path }
        next
      }
      {
        for (i = 7; i < NF && $i != "-"; i++) { }
        if (kind == "v2" && $(i + 1) != "cgroup2") next
        if (kind == "v1" && ($(i + 1) != "cgroup" || ("," $(i + 3) ",") !~ /,memory,/)) next
        root = unescape($4)
        if (root == "/") root = ""
        if (job != root && index(job, root "/") != 1) next
        if (found && length(root) >= length(best)) next
        best = root; mount = unescape($5); found = 1
      }
      END {
        if (kind == "") exit
        print kind
        if (!found) { print "-"; exit }
        print mount substr(job, length(best) + 1)
        print mount
      }
    ' "$proc/cgroup" "$proc/mountinfo" > "$work/memory-cgroup"; then
      { IFS= read -r kind; IFS= read -r at; IFS= read -r top; } < "$work/memory-cgroup" || true
    else
      unknown="its cgroups couldn't be read"
    fi
  fi
  case $kind in
    v1) max=memory.limit_in_bytes holds=memory.usage_in_bytes cache=total_inactive_file ;;
    v2) max=memory.max holds=memory.current cache=inactive_file ;;
  esac
  if [ -n "$kind" ] && [ -z "$unknown" ]; then
    if [ "$at" = - ]; then
      unknown="its memory cgroup ($kind) isn't mounted here"
    elif [ ! -d "$at" ]; then
      unknown="its memory cgroup ($kind) isn't at $at"
    else
      # The job's cgroup and each above it to the mount's, each
      # limit (none past 2^62, v1's "none") less what it holds.
      at=${at%/} top=${top%/}
      # A cgroup without the limit's file has none (the root, or
      # one whose parent doesn't hand it the controller); one
      # whose file is there but doesn't read as a number or "max"
      # has a limit nobody can read.
      number() { case $1 in '' | *[!0-9]*) return 1 ;; esac; }
      while [ -z "$unknown" ]; do
        if [ -e "$at/$max" ]; then
          limit=$(cat "$at/$max") || limit=''
          if [ "$limit" = max ]; then
            :
          elif ! number "$limit"; then
            unknown="$at/$max can't be read"
          elif [ "$limit" -lt $(( 1 << 62 )) ]; then
            if held=$(cat "$at/$holds") && inactive=$(awk -v k="$cache" '$1 == k { print $2; got = 1 } END { exit !got }' "$at/memory.stat") \
                && number "$held" && number "$inactive"; then
              left=$(( limit - held + inactive ))
              [ "$left" -ge "$free" ] || free=$left
            else
              unknown="$at has a limit but doesn't say what it holds"
            fi
          fi
        fi
        [ -n "$at" ] && [ "$at" != "$top" ] || break
        at=${at%/*}
      done
    fi
  fi
  if [ -n "$unknown" ]; then
    echo "this job's memory limit is unknown ($unknown): one evaluation at a time"
    free=0
  fi
  side=$(nproc)
  room=$(( free / 2147483648 ))
  [ "$room" -ge "$side" ] || side=$room
  [ "$side" -ge 1 ] || side=1
}
