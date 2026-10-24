# advisory-watch: notice new vulnerabilities in what the machine and its
# projects actually run, and get a Claude on each one the same hour.
# (Everything in capitals comes from ../updates.nix.)
#
#   advisory-watch          one pass (hourly; the system scan runs daily)
#   advisory-watch report   what it has seen lately
#
# Sources:
#   - CISA's Known Exploited Vulnerabilities list: being exploited right now.
#   - vulnix on the running system (NVD): CVSS 7+ only, the kernel aside (its
#     fixes arrive with every LTS bump, and the exploited ones are on CISA's list).
#   - osv-scanner over every project in SRC (crates, npm, PyPI, Go...).
#
# Plain code does the filtering: only findings it hasn't seen before, only for
# things that are really installed. What's left becomes a Claude job, in the
# machine's own project or in the project, which decides if it's real and
# fixes it (patching the machine ahead of the NixOS channel when it has to). Each source's first pass (and a
# new project's) only records what's there, so a fresh install doesn't start
# with a hundred jobs; Renovate's security PRs cover what was already open.

[ "$(id -u)" = 0 ] || exec sudo "$0" "$@"

state=$DIR/advisories
src=$SRC
mkdir -p "$state"
touch "$state/seen"

as_user() { runuser -u "$BURROW_USER" -- env HOME="$HOME_DIR" XDG_RUNTIME_DIR="/run/user/$USER_UID" "$@"; }

if [ "${1:-}" = report ]; then
  tail -n 50 "$state/log" 2>/dev/null || echo "nothing seen yet"
  exit 0
fi

read -r -a checks <<<"$CHECKS"
found=$(mktemp)
trap 'rm -f "$found"' EXIT

# Record KEY; succeed only if it's new.
fresh() {
  grep -qxF "$1" "$state/seen" && return 1
  echo "$1" >>"$state/seen"
  return 0
}
# A source's first pass only records (see above); the marker ends it.
quiet() { [ ! -e "$state/baseline-$1" ]; }
baselined() { touch "$state/baseline-$1"; }

# Package names in the running system, lower-case, versions stripped.
names=$(nix-store -qR /run/current-system | sed -E 's|^/nix/store/[a-z0-9]{32}-||; s/-[0-9].*$//' |
  tr '[:upper:]' '[:lower:]' | sort -u)

# 1. Known exploited, matched by product name against what's installed.
# cisa.gov refuses some hosting providers' addresses (403); CISA's own GitHub
# copy is the same list.
if kev=$(curl -fsS --max-time 60 https://www.cisa.gov/sites/default/files/feeds/known_exploited_vulnerabilities.json 2>/dev/null ||
  curl -fsS --max-time 60 https://raw.githubusercontent.com/cisagov/kev-data/develop/known_exploited_vulnerabilities.json); then
  # Only a match is recorded as seen: a product installed later still gets
  # its known-exploited CVEs.
  jq -r '.vulnerabilities[] | def n: ascii_downcase | gsub("[^a-z0-9-]"; "");
    [.cveID, (.vendorProject | n), (.product | n), .vulnerabilityName] | @tsv' <<<"$kev" |
    while IFS=$'\t' read -r cve v p title; do
      grep -qxF "$p" <<<"$names" || grep -qxF "$v-$p" <<<"$names" ||
        { [ "$p" = kernel ] && [ "$v" = linux ]; } || continue
      fresh "kev:$cve" || continue
      ! quiet kev || continue
      echo "$PROJECT	$cve (KNOWN EXPLOITED): $title" >>"$found"
    done || true
  baselined kev
fi

# 2. vulnix, once a day.
if [ ! -e "$state/vulnix-day" ] || [ "$(cat "$state/vulnix-day")" != "$(date +%F)" ]; then
  if out=$(vulnix --system --json 2>/dev/null) || [ -n "$out" ]; then
    date +%F >"$state/vulnix-day"
    jq -r '.[] | select(.pname | test("^linux($|-)") | not) | . as $p | .affected_by[] as $c
      | (($p.cvssv3_basescore[$c]) // 0) as $s | select($s >= 7)
      | [$c, $p.name, $s] | @tsv' <<<"$out" 2>/dev/null |
      while IFS=$'\t' read -r cve pkg score; do
        fresh "vulnix:$cve:$pkg" || continue
        ! quiet vulnix || continue
        echo "$PROJECT	$cve in $pkg (CVSS $score)" >>"$found"
      done || true
    baselined vulnix
  fi
fi

# 3. Every project's lockfiles.
for d in "$src"/*/; do
  p=$(basename "$d")
  [ -d "$d/.git" ] || continue
  out=$(as_user osv-scanner scan source --recursive --format json "$d" 2>/dev/null) || [ -n "$out" ] || continue
  jq -r '.results[]?.packages[]? | .package as $k | .groups[]? as $g
    | [($g.ids | join(",")), $k.name, $k.version, ($g.max_severity // "?")] | @tsv' <<<"$out" 2>/dev/null |
    while IFS=$'\t' read -r ids pkg ver sev; do
      fresh "osv:$p:$ids:$pkg" || continue
      ! quiet "osv-$p" || continue
      echo "$p	$ids in $pkg $ver (severity $sev)" >>"$found"
    done || true
  baselined "osv-$p"
done

[ -s "$found" ] || exit 0
sed "s/^/$(date -Is) /" "$found" >>"$state/log"

# One job per project with everything new for it.
kernel=
[ -z "$BOOT" ] || kernel=" If the fix is in the kernel, \`sudo $BOOT trial\` once it's confirmed."
cut -f1 "$found" | sort -u | while read -r p; do
  list=$(awk -F'\t' -v p="$p" '$1 == p { print "- " $2 }' "$found")
  if [ "$p" = "$PROJECT" ]; then
    task="advisory-watch found new vulnerabilities in what $HOST runs:
$list

For each one: does it really affect $HOST as it runs now? Check the version and the patches nixpkgs applies; vulnix and name matching both over-report. If the $CHANNEL channel already carries the fix, $UPDATE deploys it on its next run (see \`$UPDATE status\`). If it doesn't, and it matters (known exploited, reachable from the network, or CVSS 9+), patch $HOST now: an overlay in the flake that applies the upstream fix (system.replaceDependencies for a library, so the world doesn't rebuild), noted in docs/patches.md with the CVE and the nixpkgs version that will carry the fix, so the patch can go once the channel has it. Work in your own worktree (git -C $FLAKE worktree add $SCRATCH/fix-\$(date +%s) origin/main), never in $FLAKE itself. Build and run \`nix build -L ${checks[*]/#/.#checks.$SYSTEM.}\`, push to main, then \`sudo systemctl start --no-block $UPDATE\` to deploy it now.$kernel Write one line per CVE (affected or not, and what you did) to $state/verdicts.md. If something needs $OWNER, run \`$NOTIFY open advisory \"title\" \"what they need to do\"\`."
  else
    task="advisory-watch found new vulnerabilities in this project's dependencies (osv-scanner):
$list

Renovate opens security PRs for these on its own, without the usual wait. Make sure each fix lands today: if Renovate's PR exists, get its CI green and let it merge; if Renovate can't fix it (a transitive dependency, no fixed version yet, a breaking bump), make the fix yourself on a branch and open a PR. If a vulnerability can't affect this project (dev-only tooling, code path not used), say why in one line instead. If something needs $OWNER, run \`$NOTIFY open advisory-$p \"title\" \"what they need to do\"\`."
  fi
  if id=$(as_user proj run --first "$p" "$task"); then
    echo "$(date -Is) $p: handed to Claude, proj log $id" >>"$state/log"
  fi
done
