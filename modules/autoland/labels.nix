# The labels the steward reads and puts on PRs (burrow.autoland.labels), and
# burrow-upkeep keeps in each repo. Part of burrow.autoland, which imports it.
{ lib, ... }:
let
  inherit (lib) mkOption types;
in
{
  options.burrow.autoland.labels = {
    hold = mkOption {
      type = types.str;
      default = "hold";
      description = ''
        A PR with this label is paused: its status goes back to pending and
        nothing merges it. For a wait on something else (another PR, a
        deploy); one that waits for you takes the owner label, which lists
        it for you.
      '';
    };
    owner = mkOption {
      type = types.str;
      default = "needs-owner";
      description = "Marks a PR that waits for you; take it off to let the steward carry on.";
    };
    first = mkOption {
      type = types.str;
      default = "first";
      description = ''
        A PR with this label goes ahead of every PR without it in its repo's
        line for proofSlots, and the oldest first among those with it: for a
        fix the rest of the line waits on, opened after them.
      '';
    };
    batch = mkOption {
      type = types.str;
      default = "upstream batch";
      description = ''
        Renovate puts it on the one PR that moves every pin on a branch of a
        trusted owner's repo (burrow.updates.renovate.trustedOwners), and the
        steward lands that PR itself once it's green and settled. Renovate
        only merges it on a run that finds the branch quiet for half an
        hour, which a branch merged to all day never is.
      '';
    };
    # What a PR waits on, where it shows at a glance: the steward puts one
    # on with the status that says it and takes it off once that wait is
    # over (autoland/prs.py). Its status says the rest: until when, how
    # many are ahead, which reviewer.
    waiting = {
      releaseAge = mkOption {
        type = types.str;
        default = "waiting: release age";
        description = "On a lock file refresh none of whose updates is old enough to take yet (the minimum release age).";
      };
      proofSlot = mkOption {
        type = types.str;
        default = "waiting: proof slot";
        description = "On a PR waiting its turn for one of its repo's proofSlots.";
      };
      reviewer = mkOption {
        type = types.str;
        default = "waiting: reviewer";
        description = "On a PR that can't settle until a reviewer out of service is back.";
      };
      train = mkOption {
        type = types.str;
        default = "waiting: train";
        description = ''
          On a PR in its repo's merge train, until it lands or leaves. It's
          Renovate's `stopUpdatingLabel` too: a push to a PR in the train
          throws away the candidate it rides in, gate and all.
        '';
      };
    };
  };
}
