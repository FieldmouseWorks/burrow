# agent: one disposable microVM per task, on its own ZFS clone of the data
# dataset (burrow.agents.dataDataset, mounted at DATA). ../agents.nix sets the
# AGENT_* names above this line.
#
#   agent new PROJECT [--with DATA/PATH]... [--open]
#                               a fresh sandbox for PROJECT; prints its id
#   agent claude ID [ARGS...]   Claude inside it, in the project, no prompts
#   agent sh ID [COMMAND...]    a shell inside it (or run one command); starts
#                               it first if it was stopped
#   agent ls                    sandboxes: state, project, disk used, expiry
#   agent fetch ID              its branches and its uncommitted work, into the
#                               project on the host under refs/agents/ID/
#   agent path ID               its copy of the project, as seen from the host
#   agent stop ID / start ID    power it off (the clone stays) / back on
#   agent rm ID                 stop it, save its work, delete it in a day
#   agent keep ID [DAYS]        undo an rm; keep it DAYS (default 7) from now
#   agent gc                    what the hourly timer runs
#
# The VM sees the project (a clone, so nothing it does touches the real one),
# its own home, and any --with paths under DATA (also from the clone). Inside,
# Claude runs with permissions bypassed and the user has sudo: the VM is the
# boundary. It reaches the network only through the egress proxy, which allows
# the package registries, GitHub and Anthropic (burrow.agents.allowlist);
# --open lifts the allowlist for that VM. It holds no GitHub token: its work
# comes back with `agent fetch`, and you push from the host.
#
# A sandbox nobody has used for 7 days is saved (agent fetch) and deleted by
# `agent gc`; one left running unused for a day is stopped first.

root=$AGENT_ROOT
src=$AGENT_SRC
user=$AGENT_USER
data=$AGENT_DATA
# The projects folder, relative to the data dataset (and so to each clone).
rel=${src#"$data"/}
key=$AGENT_HOME/.ssh/agent_ed25519
day=86400

die() { echo "agent: $*" >&2; exit 1; }
dir() { [ -f "$root/${1:?agent id}/project" ] || die "no such sandbox: $1"; echo "$root/$1"; }
unit() { echo "microvm@$1.service"; }
running() { systemctl -q is-active "$(unit "$1")"; }
# The VM and the virtiofsd's serving it, which otherwise outlive it and keep
# its directory busy.
stop_vm() { systemctl stop "$(unit "$1")" "microvm-virtiofsd@$1.service" "agent-shares@$1.service"; }
as_root() { [ "$(id -u)" = 0 ] || exec /run/wrappers/bin/sudo -n "$0" "$@"; }
as_user() { [ "$(id -un)" = "$user" ] || exec runuser -u "$user" -- "$0" "$@"; }

# ssh_to ID [SSH OPTIONS...] -- [COMMAND]: ssh into a sandbox as the user, over
# vsock. With $ssh_timeout set, give up after that many seconds.
ssh_to() {
  local d cid opts=()
  d=$(dir "$1")
  cid=$(cat "$d/cid")
  shift
  while [ $# -gt 0 ] && [ "$1" != -- ]; do opts+=("$1"); shift; done
  [ $# -eq 0 ] || shift
  ${ssh_timeout:+timeout "$ssh_timeout"} ssh -i "$key" "${opts[@]}" -o BatchMode=yes -o LogLevel=ERROR \
    -o ConnectTimeout=5 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ProxyCommand="socat - VSOCK-CONNECT:$cid:22" "$user@agent-$cid" "$@"
}

wait_ready() {
  local i
  for i in $(seq 1 240); do
    ssh_timeout=10 ssh_to "$1" -- true 2>/dev/null && return 0
    running "$1" || break
    sleep 0.5
  done
  # Once more, out loud, so the reason is on screen.
  ssh_timeout=10 ssh_to "$1" -o LogLevel=DEBUG1 -- true 2>&1 | tail -n 25 >&2 || true
  die "$1 did not come up; see: journalctl -u $(unit "$1")"
}

expires() {
  local d=$root/$1
  if [ -e "$d/removed" ]; then
    echo $(($(stat -c %Y "$d/removed") + day))
  else
    echo $(($(stat -c %Y "$d/last-used") + $(cat "$d/ttl-days") * day))
  fi
}

# The per-VM QEMU arguments (read by the runner from ./qemu-args at every
# start): the network, the vsock address, the shares and the credentials.
write_args() {
  local d=$root/$1 cid fwd i=0 tag
  cid=$(cat "$d/cid")
  if [ "$(cat "$d/net")" = open ]; then fwd=$AGENT_FWD_OPEN; else fwd=$AGENT_FWD_FILTERED; fi
  # Fresh each start, so a token the host has since dropped doesn't linger.
  rm -f "$d/claude-token"
  if [ -n "$AGENT_TOKEN_FILE" ] && [ -s "$AGENT_TOKEN_FILE" ]; then
    install -m 0400 -o microvm -g kvm "$AGENT_TOKEN_FILE" "$d/claude-token"
  fi
  {
    printf -- '-fw_cfg name=opt/io.systemd.credentials/agent.mounts,file=mounts '
    printf -- '-fw_cfg name=opt/io.systemd.credentials/agent.id,string=%s ' "$1"
    [ ! -s "$d/claude-token" ] ||
      printf -- '-fw_cfg name=opt/io.systemd.credentials/agent.claude-token,file=claude-token '
    printf -- '-device vhost-vsock-pci,guest-cid=%s ' "$cid"
    # restrict=on: no route out and no way to the host, except the one
    # forwarded address that is the proxy.
    printf -- '-netdev user,id=agentnet,restrict=on,guestfwd=tcp:10.0.2.100:3128-cmd:%s ' "$fwd"
    printf -- '-device virtio-net-pci,netdev=agentnet,romfile=,mac=02:00:%02x:%02x:%02x:%02x ' \
      $((cid >> 24 & 255)) $((cid >> 16 & 255)) $((cid >> 8 & 255)) $((cid & 255))
    while IFS=$'\t' read -r tag _ _ _; do
      printf -- '-chardev socket,id=agentfs%s,path=%s.sock ' "$i" "$tag"
      printf -- '-device vhost-user-fs-pci,chardev=agentfs%s,tag=%s ' "$i" "$tag"
      i=$((i + 1))
    done <"$d/shares"
    echo
  } >"$d/qemu-args"
}

# Save a sandbox's work into the real project: every branch (as
# refs/agents/ID/heads/*), plus everything in its working tree (uncommitted and
# untracked, minus ignored) as one commit on top of its HEAD, at
# refs/agents/ID/worktree. The VM could have written anything into its repo's
# config (hooks, filters, fsmonitor), so nothing here runs git with that config
# where it could execute code: the snapshot is made in a scratch repo that
# only borrows the clone's objects.
# Returns 2 when there is nothing to save to (not a repository of its own).
save_work() {
  local id=$1 d p repo clone t head ref obj
  d=$(dir "$id")
  p=$(cat "$d/project")
  repo=$src/$p
  clone=$d/data/$rel/$p
  # A worktree's .git is a file pointing into the real repo on the host.
  [ -d "$clone/.git" ] && [ -d "$repo/.git" ] || return 2
  t=$(mktemp -d -p "$d" save.XXXX)
  git init -q --bare "$t"
  echo "$clone/.git/objects" >"$t/objects/info/alternates"
  cp "$clone/.git/info/exclude" "$t/info/exclude" 2>/dev/null || true
  git --git-dir="$clone/.git" for-each-ref --format='%(objectname) %(refname)' refs/heads |
    while read -r obj ref; do git --git-dir="$t" update-ref "$ref" "$obj"; done
  # A repository with no commits yet still has its files: they become a root
  # commit.
  head=$(git --git-dir="$clone/.git" rev-parse -q --verify 'HEAD^{commit}' || true)
  GIT_INDEX_FILE=$t/index git --git-dir="$t" --work-tree="$clone" add -A
  obj=$(git --git-dir="$t" commit-tree --no-gpg-sign "$(GIT_INDEX_FILE=$t/index git --git-dir="$t" write-tree)" ${head:+-p "$head"} \
    -m "agent $id: working tree as of $(date -Is)")
  git --git-dir="$t" update-ref refs/worktree "$obj"
  git -C "$repo" fetch -q --no-write-fetch-head "$t" "+refs/heads/*:refs/agents/$id/heads/*" \
    "+refs/worktree:refs/agents/$id/worktree"
  rm -rf "$t"
}

destroy() {
  stop_vm "$1"
  zfs destroy -r "$AGENT_DATASET/$1"
  zfs destroy "$AGENT_DATA_DATASET@agent-$1"
  # zfs usually removes the mountpoint itself.
  [ ! -d "$root/$1" ] || rmdir "$root/$1"
}

case "${1:-ls}" in
  new)
    as_user "$@"
    install -d -m 0700 "$(dirname "$key")"
    [ -f "$key" ] || ssh-keygen -q -t ed25519 -N '' -C "agent@$(uname -n)" -f "$key"
    id=$(/run/wrappers/bin/sudo -n "$0" _new "${@:2}") || exit 1
    wait_ready "$id"
    echo "$id"
    ;;
  _new)
    as_root "$@"
    p=${2:?usage: agent new PROJECT [--with $data/PATH]... [--open]}
    shift 2
    # A folder directly in the projects folder, nothing else: the name goes
    # into the share paths.
    case $p in */* | . | .. | "") die "not a project name: $p" ;; esac
    [ -d "$src/$p" ] || die "no such project: $src/$p"
    net=filtered
    withs=()
    while [ $# -gt 0 ]; do
      case $1 in
        --open) net=open ;;
        --with)
          w=$(realpath -m "${2:?--with needs a path under $data}")
          case $w in "$data"/?*) ;; *) die "--with takes a path under $data, not $2" ;; esac
          [ -e "$w" ] || die "no such path: $w"
          withs+=("$w")
          shift
          ;;
        *) die "unknown option: $1" ;;
      esac
      shift
    done

    slug=$(printf '%s' "$p" | tr '[:upper:]' '[:lower:]' | tr -c 'a-z0-9' '-' | tr -s '-' | sed 's/^-//; s/-$//')
    exec 9>"$root/.lock"
    flock 9
    n=$(($(cat "$root/.counter" 2>/dev/null || echo 0) + 1))
    echo "$n" >"$root/.counter"
    flock -u 9
    id=${slug:-agent}-$n
    d=$root/$id

    # The clone: all of the data dataset as of now, for free until something
    # changes.
    zfs snapshot "$AGENT_DATA_DATASET@agent-$id"
    zfs create "$AGENT_DATASET/$id"
    zfs clone "$AGENT_DATA_DATASET@agent-$id" "$AGENT_DATASET/$id/data"
    chown "$user:kvm" "$d"
    chmod 0770 "$d"

    install -d -m 0700 -o "$user" -g "$AGENT_GROUP" "$d/home" "$d/home/.ssh"
    install -m 0600 -o "$user" -g "$AGENT_GROUP" "$key.pub" "$d/home/.ssh/authorized_keys"
    # Claude inside starts straight into work: no onboarding, folder trusted.
    jq -n --arg p "$src/$p" '{hasCompletedOnboarding: true, projects: {($p): {hasTrustDialogAccepted: true}}}' \
      >"$d/home/.claude.json"
    chown "$user:$AGENT_GROUP" "$d/home/.claude.json"

    printf '%s\n' "$p" >"$d/project"
    echo $((1000 + n)) >"$d/cid"
    echo "$net" >"$d/net"
    echo 7 >"$d/ttl-days"
    touch "$d/last-used"
    # Shares: tag, path on the host, path in the VM. Mounts: what the VM sees.
    {
      printf 'share0\t%s\t%s\n' "$d/data/$rel/$p" "$src/$p"
      printf 'share1\t%s\t%s\n' "$d/home" "$AGENT_HOME"
      i=2
      for w in "${withs[@]}"; do
        printf 'share%s\t%s\t%s\n' "$i" "$d/data/${w#"$data"/}" "$w"
        i=$((i + 1))
      done
    } >"$d/shares"
    cut -f1,3 "$d/shares" >"$d/mounts"
    chown "$user:kvm" "$d"/{project,cid,net,ttl-days,last-used,shares,mounts}
    ln -s "$AGENT_RUNNER" "$d/current"
    write_args "$id"

    systemctl start "$(unit "$id")"
    echo "$id"
    ;;
  claude)
    as_user "$@"
    exec "$0" sh "${2:?usage: agent claude ID [ARGS...]}" claude "${@:3}"
    ;;
  sh)
    as_user "$@"
    id=${2:?usage: agent sh ID [COMMAND...]}
    d=$(dir "$id")
    shift 2
    # Stopped (by gc, a reboot, or agent stop): just bring it back.
    running "$id" || /run/wrappers/bin/sudo -n "$0" start "$id"
    touch "$d/last-used"
    cd_cmd="cd $(printf '%q' "$src/$(cat "$d/project")")"
    tty=()
    if [ -t 0 ] && [ -t 1 ]; then tty=(-t); fi
    if [ $# -eq 0 ]; then
      ssh_to "$id" "${tty[@]}" -- "$cd_cmd && exec bash -l"
    else
      ssh_to "$id" "${tty[@]}" -- "$cd_cmd && exec bash -lc $(printf '%q' "$(printf '%q ' "$@")")"
    fi
    ;;
  ls)
    now=$(date +%s)
    printf '%-24s %-8s %-20s %8s  %s\n' ID STATE PROJECT USED EXPIRES
    for d in "$root"/*/; do
      id=$(basename "$d")
      [ -f "$d/project" ] || continue
      if running "$id"; then state=running
      elif [ -e "$d/removed" ]; then state=removed
      else state=stopped
      fi
      used=$(zfs get -Hp -o value used "$AGENT_DATASET/$id" 2>/dev/null | numfmt --to=iec || echo '?')
      left=$(($(expires "$id") - now))
      printf '%-24s %-8s %-20s %8s  in %sh\n' "$id" "$state" "$(cat "$d/project")" "$used" $((left / 3600))
    done
    ;;
  path)
    d=$(dir "${2:?usage: agent path ID}")
    echo "$d/data/$rel/$(cat "$d/project")"
    ;;
  fetch)
    as_user "$@"
    id=${2:?usage: agent fetch ID}
    rc=0
    save_work "$id" || rc=$?
    [ "$rc" != 2 ] || die "$(cat "$(dir "$id")/project") is not a git repository of its own; nothing to fetch into"
    [ "$rc" = 0 ] || exit "$rc"
    git -C "$src/$(cat "$root/$id/project")" for-each-ref --format='%(refname) %(objectname:short) %(subject)' "refs/agents/$id/"
    ;;
  start)
    as_root "$@"
    id=${2:?usage: agent start ID}
    dir "$id" >/dev/null
    rm -f "$root/$id/removed"
    touch "$root/$id/last-used"
    write_args "$id"
    systemctl start "$(unit "$id")"
    runuser -u "$user" -- "$0" _wait "$id"
    ;;
  _wait) as_user "$@"; wait_ready "$2" ;;
  stop)
    as_root "$@"
    dir "${2:?usage: agent stop ID}" >/dev/null
    stop_vm "$2"
    ;;
  rm)
    as_root "$@"
    id=${2:?usage: agent rm ID}
    d=$(dir "$id")
    stop_vm "$id"
    runuser -u "$user" -- "$0" fetch "$id" >/dev/null 2>&1 || true
    touch "$d/removed"
    echo "$id stopped; deleted in a day unless you run: agent keep $id"
    ;;
  keep)
    id=${2:?usage: agent keep ID [DAYS]}
    d=$(dir "$id")
    rm -f "$d/removed"
    echo "${3:-7}" >"$d/ttl-days"
    touch "$d/last-used"
    ;;
  gc)
    as_root "$@"
    now=$(date +%s)
    for d in "$root"/*/; do
      id=$(basename "$d")
      [ -f "$d/project" ] || continue
      if running "$id"; then
        idle=$((now - $(stat -c %Y "$d/last-used")))
        # In use: someone is connected to it right now.
        pgrep -f "VSOCK-CONNECT:$(cat "$d/cid"):22" >/dev/null && continue
        if [ -e "$d/removed" ] || [ "$idle" -gt "$day" ]; then
          echo "$id: stopping (unused for $((idle / 3600))h)"
          stop_vm "$id"
        fi
        continue
      fi
      [ "$now" -ge "$(expires "$id")" ] || continue
      rc=0
      runuser -u "$user" -- "$0" fetch "$id" >/dev/null 2>&1 || rc=$?
      if [ "$rc" = 0 ] || [ -e "$d/removed" ]; then
        echo "$id: work saved to refs/agents/$id in $(cat "$d/project"); deleting"
        destroy "$id"
      else
        echo "$id: expired, but its work couldn't be saved (fetch exit $rc); keeping it" >&2
      fi
    done
    ;;
  *)
    echo "usage: agent new|claude|sh|ls|fetch|path|stop|start|rm|keep|gc" >&2
    exit 2
    ;;
esac
