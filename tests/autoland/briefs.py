"""The steward's briefs that carry burrow-brief's policy text (the fix
job's findings, the judge's criteria) are byte-identical to what they were
before the text moved there, for fixed inputs: the goldens in
crates/burrow-brief/tests/golden, rendered from main before the move. A
verdict line's seal is a MAC under this run's own key, and the jobs dir
is in this run's own temporary dir, so both are set aside.
BRIEF_GOLDEN_WRITE=1 writes them instead."""

import os
import re

from harness import BRIEF_GOLDEN, H, head, tmp
import tasks as T

pr = {"number": 6, "title": "t", "headRefName": "x", "headRefOid": head, "baseRefName": "main",
      "body": "Adds a helper.\n\nWhy not Rust: quicker to write"}
rendered = {
    "fix": H.fix_task("sam/r", pr, "2 open review threads"),
    "judge-reviewed": H.judge_task("sam/r", pr, "it touches .github/", False),
    "judge-past-limit-no-ci": H.judge_task("sam/r", pr, "it went past its round limit", True, seen=T.PAST_LIMIT),
    "judge-dependency": H.judge_task("sam/r", pr, "it's a dependency update", False, seen=T.DEP_SEEN),
}
for name, text in rendered.items():
    text = text.replace(tmp, "TMP")
    text = re.sub(r"(<!-- autoland-judgment: \S+ \S+ )\S+( -->)", r"\1SEAL\2", text)
    path = os.path.join(BRIEF_GOLDEN, f"{name}.txt")
    if os.environ.get("BRIEF_GOLDEN_WRITE"):
        with open(path, "w") as f:
            f.write(text)
        continue
    with open(path) as f:
        assert f.read() == text, (name, text)
