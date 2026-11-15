# Where the steward (autoland.nix) and the reviewers' service (review.nix)
# meet: the reviewers' user, the review queue and the snapshots it reads,
# and the kick that starts a repo's pass. Given the machine's config.
config:
let
  inherit (config.burrow) stateDir;
in
{
  # The user the review agents run as: it has no GitHub login, and reads
  # only the snapshots the steward takes (secrets paths left out).
  reviewer = "autoland-review";
  reviewerHome = "${stateDir}/autoland-review";
  reviewDir = "${stateDir}/autoland/review";
  # The snapshots' checkouts (reviewers.py TREES): a cache, taken again from
  # GitHub whenever one is missing, so out of stateDir and whatever snapshots
  # the machine keeps of it. On a machine whose root is wiped at boot, a boot
  # empties it too.
  snapDir = "/var/cache/autoland/snap";
  # The review producer's fresh looks (autoland/review-producer.nix): each
  # one's snapshot, which the producer takes and the reviewers' group
  # reads, and its session's home, which the reviewers' service makes.
  # Nothing in it is resumed; a cache too.
  freshDir = "/var/cache/autoland/fresh";
  # Where a repo's pass is kicked: the reviewers' service kicks it when an
  # answer comes back, the steward when it books one.
  stewardKick = "${config.burrow.events.kick}/autoland";
}
