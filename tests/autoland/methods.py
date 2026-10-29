"""A settled PR is merged with a method its base's rules allow: the first of
squash, merge and rebase that the repo's settings, every ruleset on the base
and its classic protection let through (a linear history lets no merge
commit through), since GitHub refuses any other from the App. A private repo
on a free plan has no rules, so its settings decide. With none, or no word
from GitHub on the rules, nothing is merged and the merge is tried again
later, as for any refused merge."""

from harness import PROTECTION, RULES, C, T, clean, fresh_pr_state, info, merges, pr_state, run, stand_in

stand_in()

ALL = ["merge", "squash", "rebase"]


def pull_request(*methods):
    return {"type": "pull_request", "parameters": {"allowed_merge_methods": list(methods)}}


def merged_with():
    return [x.split("merge_method=")[1].split()[0] for x in merges()]


# A repo whose settings allow every method.
kept = dict(info)
info.update(squashMergeAllowed=True, mergeCommitAllowed=True, rebaseMergeAllowed=True)
# A base whose rules allow every method (or say nothing of methods) takes
# the repo's first: squash.
for rules in ([], [pull_request(*ALL)], [{"type": "pull_request", "parameters": {}}], [{"type": "deletion"}]):
    RULES["main"] = rules
    fresh_pr_state()
    run(clean(), 900)
    assert merged_with() == ["squash"], (rules, merges())
# A legacy ruleset beside the baseline that allows merge commits alone
# narrows them: the merge is a merge commit.
RULES["main"] = [pull_request(*ALL), {"type": "deletion"}, pull_request("merge")]
fresh_pr_state()
run(clean(), 900)
assert merged_with() == ["merge"], merges()
# A linear history lets no merge commit through: what's left is rebase,
# or nothing.
RULES["main"] = [pull_request("merge", "rebase"), {"type": "required_linear_history"}]
fresh_pr_state()
run(clean(), 900)
assert merged_with() == ["rebase"], merges()
RULES["main"] = [{"type": "required_linear_history"}, pull_request("merge")]
fresh_pr_state()
run(clean(), 900)
assert merges() == [], merges()
# So does a classic protection's, on the PR's own base.
RULES["main"] = [pull_request("merge", "rebase")]
for linear, want in ((True, ["rebase"]), (False, ["merge"])):
    PROTECTION["main"] = {"required_linear_history": {"enabled": linear}}
    fresh_pr_state()
    run(clean(), 900)
    assert merged_with() == want, (linear, merges())
PROTECTION["main"] = {"required_linear_history": {"enabled": True}}
RULES["release%2F1.0"] = [pull_request("merge", "rebase")]
fresh_pr_state()
run(clean(base="release/1.0"), 900)
assert merged_with() == ["merge"], merges()
PROTECTION.clear()
del RULES["release%2F1.0"]
# A private repo on a free plan, whose plan offers no rules, takes its
# settings' first.
PLAN = "gh: Upgrade to GitHub Pro or make this repository public to enable this feature. (HTTP 403)"
RULES["main"] = RuntimeError(f"gh api repos/sam/r/rules/branches/main?per_page=100: {PLAN}")
PROTECTION["main"] = RuntimeError(f"gh api repos/sam/r/branches/main/protection: {PLAN}")
fresh_pr_state()
run(clean(), 900)
assert merged_with() == ["squash"], merges()
PROTECTION.clear()
# The rules read are the PR's own base's, a slash in its name kept whole.
RULES["main"] = [pull_request("rebase")]
RULES["release%2F1.0"] = [pull_request("squash")]
fresh_pr_state()
run(clean(base="release/1.0"), 900)
assert merged_with() == ["squash"], merges()
# One the repo's settings don't allow (only squash, now) isn't tried, and
# neither is any when GitHub won't say what the rules are: nothing merges,
# and it's a merge that didn't go through, tried again a pass later.
info.update(mergeCommitAllowed=False, rebaseMergeAllowed=False)
DENIED = "Resource not accessible by integration (HTTP 403)"
for rules, protection in (([pull_request("merge", "rebase")], None),
                          (C.Unavailable("gh api repos/sam/r/rules/branches/main: HTTP 502"), None),
                          (RuntimeError(f"gh api repos/sam/r/rules/branches/main: {DENIED}"), None),
                          ([], C.Unavailable("gh api repos/sam/r/branches/main/protection: HTTP 502")),
                          ([], RuntimeError(f"gh api repos/sam/r/branches/main/protection: {DENIED}"))):
    RULES["main"] = rules
    if protection:
        PROTECTION["main"] = protection
    fresh_pr_state()
    status, _ = run(clean(), 900)
    assert merges() == [] and "state=success" in status and not pr_state().get("landed"), (rules, merges(), status)
    assert C.WAKE["sam/r"] <= T + 900 + 2 * 60, (rules, C.WAKE)
    RULES["main"] = []
    PROTECTION.clear()
    run(clean(), 960)
    assert merged_with() == ["squash"], (rules, merges())
info.update(kept)
RULES.clear()
fresh_pr_state()
print("merge methods: ok")
