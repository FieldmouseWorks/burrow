"""Admission and deadline handoff for a job's first checkout.

The Rust checkout supervisor owns process cleanup and atomic publication.
"""
import subprocess
import time


def checkout(repo):
    """Adopt a renamed clone, report an occupied destination, or publish a
    completed clone. A failed attempt never supplies a project to a job."""
    import common as C

    C.follow_renames()
    if C.retiring(repo):
        return None
    d = C.project_dir(repo)
    if d:
        return d
    path = C.destination(repo)
    if C.blocked(path):
        had = {"folder": path, "origin": C.origin(path)}
        C.log(f"{repo}: {C.taken_line(repo, had)[2]}")
        C.save(C.TAKEN_FILE, {**C.load(C.TAKEN_FILE, {}), repo: had})
        return None
    try:
        until = time.monotonic() + C.within(C.PASS_SECONDS)
    except C.Unavailable as e:
        C.log(f"{repo}: its clone: {e}")
        return None
    # This supervisor enforces the absolute monotonic deadline itself and
    # must finish reaping before its parent returns. An outer run(timeout=)
    # would kill the supervisor and abandon the very children it must reap.
    # No App environment: the project's clone belongs to the machine user.
    p = subprocess.run([*C.CFG["upkeep"], "checkout", repo, path, str(until)], check=False)
    return C.project_dir(repo) if p.returncode == 0 else None
