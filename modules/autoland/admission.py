"""The steward's adapter to the shared Rust admission decision."""
import json
import subprocess


class JobPaused(Exception):
    """Intentional admission hold, never a failed launch or a job id."""


def paused(repo):
    import common as C

    settings = C.CFG.get("settings")
    if settings:
        result = subprocess.run([settings["command"], settings["config"], "decision", repo],
                                capture_output=True, text=True)
        if result.returncode != 0:
            return True
        decision = json.loads(result.stdout)
        if decision["retryAt"] is not None:
            C.wake(repo, decision["retryAt"])  # ends: the scoped pause expires
        return not decision["allowed"]
    return True

