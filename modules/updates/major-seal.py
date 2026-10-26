"""major-seal: renovate-sweep's verdict on a major, sealed (../autoland/seal.py).

The autoland steward merges a Renovate major on the sweep's ok, so a
well-formed marker any Claude job could post (one quoting PR text, say)
mustn't count. A judge is handed its seals with its task; the sweep's job
picks its head itself (the one after its last push), so it seals its
verdict here, with the token its task gives it, for a PR that task handed
it:

    major-seal STATE TOKEN N ok|held SHA BASE
        prints the marker to end its comment with
    major-seal STATE --check REPO N ok|held SHA BASE MAC
        exits 0 if MAC seals that verdict, 1 if not

STATE is the sweep's state dir: its key, and seal/TOKEN, which holds the
repo the task was about and then the PRs it handed over, one a line.
"""

import hmac
import os
import re
import sys

import seal as S


def fail(why):
    print(f"major-seal: {why}", file=sys.stderr)
    sys.exit(2)


def handed(state, token):
    """The repo the task with this token was about and the PRs it handed
    over, or None."""
    if not re.fullmatch(r"[0-9a-f]{32}", token):
        return None
    try:
        repo, *prs = open(os.path.join(state, "seal", token)).read().split()
    except (FileNotFoundError, ValueError):
        return None
    return repo, prs


def main(a):
    if len(a) == 8 and a[1] == "--check":
        state, _, repo, n, verdict, head, base, mac = a
        k = S.key(os.path.join(state, "verdict.key"), create=False)
        sys.exit(0 if k and hmac.compare_digest(mac, S.major(k, repo, n, verdict, head, base)) else 1)
    if len(a) != 6:
        fail("usage: major-seal TOKEN N ok|held SHA BASE")
    state, token, n, verdict, head, base = a
    if verdict not in ("ok", "held"):
        fail(f"a verdict is ok or held, not {verdict!r}")
    if not re.fullmatch(r"[0-9a-f]{40}", head):
        fail(f"SHA is the PR's full head commit, not {head!r}")
    if not re.fullmatch(r"[^\s>]+", base):
        fail(f"BASE is the branch the PR targets, not {base!r}")
    task = handed(state, token)
    if not task or n not in task[1]:
        fail(f"that token didn't hand you PR {n}: seal only the PRs your task names, with the token it gives")
    k = S.key(os.path.join(state, "verdict.key"))
    print(f"<!-- {S.MAJOR}: {verdict} {head} on {base} {S.major(k, task[0], n, verdict, head, base)} -->")


if __name__ == "__main__":
    main(sys.argv[1:])
