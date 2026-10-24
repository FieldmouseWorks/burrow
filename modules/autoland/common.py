"""What the steward and its DeepSeek reviewer share: the config, the state
directory, and talking to GitHub through `gh`."""

import json
import os
import subprocess
import sys
import time
from datetime import datetime

CFG = json.load(open(os.environ["AUTOLAND_CONFIG"]))
DS = CFG.get("deepseek") or {"enable": False}
DRY = len(sys.argv) > 1 and sys.argv[1] == "status"
# A dry run keeps its own state, so its once-a-day reads (settings, branches)
# stay once a day instead of running every pass on the owner's API quota.
STATE = CFG["stateDir"] + ("/dry" if DRY else "")
NOW = time.time()
MIN = 60


def log(*a):
    print(*a, flush=True)


def gh(*args, input=None, check=True):
    """Run gh; return its stdout (parsed as JSON when it is JSON)."""
    p = subprocess.run(["gh", *args], input=input, capture_output=True, text=True)
    if p.returncode != 0:
        if check:
            raise RuntimeError(f"gh {' '.join(args[:3])}: {p.stderr.strip()[:300]}")
        return None
    out = p.stdout.strip()
    try:
        return json.loads(out) if out else ""
    except ValueError:
        return out


def write(*args, **kw):
    """A gh call that changes something: skipped (and logged) in status mode."""
    if DRY:
        log("  would:", "gh", " ".join(a if len(a) < 80 else a[:77] + "..." for a in args))
        return None
    return gh(*args, **kw)


def ts(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp() if s else 0


def load(name, default):
    try:
        return json.load(open(os.path.join(STATE, name)))
    except (OSError, ValueError):
        return default


def save(name, value):
    path = os.path.join(STATE, name)
    try:
        with open(path + ".tmp", "w") as f:
            json.dump(value, f, indent=1)
        os.replace(path + ".tmp", path)
    except OSError:
        if not DRY:  # `status` from a shell that can't write the state dir: fine
            raise
