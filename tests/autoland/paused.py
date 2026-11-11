"""Paused repositories never reach the steward's cloning or job startup path."""
from unittest.mock import patch

from harness import C, REAL

C.CFG["paused"] = ["SAM/R"]
with patch.object(C, "retiring", side_effect=AssertionError("admitted")):
    for role in ("steward-handed", "steward-judged", "proof"):
        assert REAL["job"]("sam/r", "must not start", clone=True, role=role) is None
    # An unlisted repository still reaches normal admission.
    try:
        REAL["job"]("sam/other", "task", role="steward-handed")
    except AssertionError as error:
        assert str(error) == "admitted"
    else:
        raise AssertionError("unlisted repository was paused")
C.CFG.pop("paused")
