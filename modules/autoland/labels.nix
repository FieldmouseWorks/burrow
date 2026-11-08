# The labels the steward reads and puts on PRs (burrow.autoland.labels), and
# burrow-upkeep keeps in each repo. Part of burrow.autoland, which imports it.
{ config, lib, ... }:
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
    # Where a PR is in the pipeline, at a glance: every status the steward
    # posts names one of these stages (autoland/stages.py), and the PR
    # carries that stage's label and none of the others, put on with the
    # status and taken off with it (autoland/prs.py). Its status says the
    # rest: who has it, until when, how many are ahead. A draft and a PR on
    # hold carry none: GitHub's draft mark and the hold label say it.
    # burrow-upkeep makes each in every repo, with its colour and
    # description.
    waiting =
      lib.mapAttrs
        (stage: d: {
          name =
            mkOption {
              type = types.str;
              default = d.name;
              description = "The label on a PR at the `${stage}` stage.";
            }
            // lib.optionalAttrs (d ? defaultText) { inherit (d) defaultText; };
          description =
            mkOption {
              type = types.str;
              default = d.description;
              description = "What the label says, in the repo's label list (GitHub keeps 100 characters).";
            }
            // lib.optionalAttrs (d ? descriptionText) { defaultText = d.descriptionText; };
          color = mkOption {
            type = types.strMatching "[0-9a-f]{6}";
            default = d.color or "c5def5";
            description = "The label's colour.";
          };
        })
        {
          review = {
            name = "waiting: review";
            description = "Its reviewers, or a Claude job judging it, are looking at this head";
          };
          reviewer = {
            name = "waiting: reviewer";
            description = "Can't settle until a reviewer out of service is back; the steward's status says which";
          };
          fix = {
            name = "waiting: fix";
            description = "A Claude job, or the session that opened it, fixes findings, red CI or a conflict";
          };
          ci = {
            name = "waiting: CI";
            description = "Reviewed clean; waits for its CI to finish";
          };
          proofSlot = {
            name = "waiting: proof slot";
            description = "Waits its turn for the machine's own checks; the steward's status says how many are ahead";
          };
          proof = {
            name = "waiting: machine checks";
            description = "The checks only the machine runs (VM tests and the like) are building or running";
          };
          releaseAge = {
            name = "waiting: release age";
            description = "Its updates are too new to take yet (the minimum release age); the status says until when";
          };
          train = {
            # Renovate's `stopUpdatingLabel` too (updates.nix): a push to a PR
            # in the train throws away the candidate it rides in, gate and all.
            name = "waiting: train";
            description = "In its merge train, which lands it; the steward's status says where. Renovate leaves it be";
          };
          merge = {
            name = "waiting: merge";
            description = "Settled and green: being merged by the steward, GitHub or its dependency bot";
          };
          owner = {
            name = "waiting: ${config.burrow.owner}";
            defaultText = lib.literalExpression ''"waiting: ''${config.burrow.owner}"'';
            description = "Waits for ${config.burrow.owner}: listed in the steward's issue of PRs waiting for them";
            descriptionText = lib.literalExpression ''"Waits for ''${config.burrow.owner}: ..."'';
            color = "b60205";
          };
          base = {
            name = "waiting: base branch";
            description = "Waits on its base: the PR it's stacked on, or CI on the red default branch it fixes";
          };
        };
  };
}
