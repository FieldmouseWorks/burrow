"""major-seal: renovate-sweep's verdict on a major, sealed (../autoland/seal.py).

The autoland steward merges a Renovate major on the sweep's ok, so a
well-formed marker any Claude job could post (one quoting PR text, say)
mustn't count. The sweep seals both verdicts on the head and base it hands
a major over on and gives its job the markers with its task, as the
steward gives a judge its seals: the job runs nothing here, so a job whose
sandbox can't see the sweep's state dir can seal all the same.

    major-seal STATE REPO N ok|held SHA BASE
        prints the marker a job ends its comment with
    major-seal STATE --check REPO N ok|held SHA BASE MAC
        exits 0 if MAC seals that verdict, 1 if not

STATE is the sweep's state dir, which holds its key.
"""

import hmac
import os
import re
import sys

import seal as S


def fail(why):
    print(f"major-seal: {why}", file=sys.stderr)
    sys.exit(2)


def main(a):
    if len(a) == 8 and a[1] == "--check":
        state, _, repo, n, verdict, head, base, mac = a
        k = S.key(os.path.join(state, "verdict.key"), create=False)
        sys.exit(0 if k and hmac.compare_digest(mac, S.major(k, repo, n, verdict, head, base)) else 1)
    if len(a) != 6:
        fail("usage: major-seal REPO N ok|held SHA BASE")
    state, repo, n, verdict, head, base = a
    if not re.fullmatch(r"[^\s/]+/[^\s/]+", repo):
        fail(f"REPO is OWNER/NAME, not {repo!r}")
    if not re.fullmatch(r"[1-9][0-9]*", n):
        fail(f"N is the PR's number, not {n!r}")
    if verdict not in ("ok", "held"):
        fail(f"a verdict is ok or held, not {verdict!r}")
    if not re.fullmatch(r"[0-9a-f]{40}", head):
        fail(f"SHA is the PR's full head commit, not {head!r}")
    if not re.fullmatch(r"[^\s>]+", base):
        fail(f"BASE is the branch the PR targets, not {base!r}")
    k = S.key(os.path.join(state, "verdict.key"))
    print(f"<!-- {S.MAJOR}: {verdict} {head} on {base} {S.major(k, repo, n, verdict, head, base)} -->")


if __name__ == "__main__":
    main(sys.argv[1:])
