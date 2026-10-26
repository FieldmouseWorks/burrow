"""The order the reviewers look in: one set to look `last` (DeepSeek, on
the machine) waits for the others in service to find a head clean."""

import os

from harness import C, POSTED, V, fresh_pr_state, head, merged, mkpr, queued, run, rv_review, tmp, worker

C.CFG["reviewers"]["deepseek"]["last"] = True

# Codex first, on its own; its findings, open or answered, keep DeepSeek
# off this head, and Codex looks again at answered ones.
fresh_pr_state()
run(mkpr(), 0)
status, _ = run(mkpr(), 600)
assert queued() == ["codex"] and "DeepSeek" not in status, (queued(), status)
worker(codex={"result": "review", "answer": {"findings": [{"path": "a.py"}]}})
run(mkpr(), 610)
assert ("codex", head, 1) in POSTED and queued() == [], (POSTED, queued())
found = [rv_review("codex", 1, at="2026-10-24T00:10:10Z")]
status, _ = run(mkpr(reviews=found, threads=[False]), 620)
assert queued() == [] and "open review thread" in status, status
status, _ = run(mkpr(reviews=found, threads=[True]), 1300)
assert queued() == ["codex"] and "Findings answered" in status, (queued(), status)
# Clean, and DeepSeek is asked: both clean, it lands.
worker()
run(mkpr(reviews=found, threads=[True]), 1310)
status, _ = run(mkpr(reviews=[*found, rv_review("codex", 0, at="2026-10-24T00:21:50Z")], threads=[True]), 1320)
assert queued() == ["deepseek"] and "DeepSeek is reviewing" in status, (queued(), status)
worker()
status, _ = run(mkpr(reviews=[*found, rv_review("codex", 0, at="2026-10-24T00:21:50Z")], threads=[True]), 1330)
assert ("deepseek", head, 0) in POSTED and merged(), (POSTED, status)

# With Codex out of service, DeepSeek looks at once.
fresh_pr_state()
C.save(V.OUT, {"codex": {"why": "usage", "until": C.NOW + 86400}})
run(mkpr(), 0)
run(mkpr(), 600)
assert queued() == ["deepseek"], queued()
os.remove(f"{tmp}/state/{V.OUT}")

C.CFG["reviewers"]["deepseek"]["last"] = False
print("reviewer order: ok")
