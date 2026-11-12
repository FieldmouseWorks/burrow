"""Durable proof state and the user manager that owns its cgroups."""
import os
import subprocess


def build_unit(jid):
    return f"autoland-{jid}.service"


def user_systemctl(*args):
    """systemctl --user, as the machine user, from a unit that doesn't set
    the user's runtime dir (proj sets it the same way)."""
    env = dict(os.environ)
    env.setdefault("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")
    return subprocess.run(["systemctl", "--user", *args], capture_output=True, text=True, env=env)


def user_systemctl_run(unit, cmd):
    """Starts cmd as unit, a transient one of the machine user's that goes
    once it ends (its end is in its build's folder), with this steward's
    config and tools."""
    env = dict(os.environ)
    env.setdefault("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")
    keep = [f"--setenv={k}={os.environ[k]}" for k in ("AUTOLAND_CONFIG", "PATH") if k in os.environ]
    return subprocess.run(["systemd-run", "--user", "--quiet", "--collect", f"--unit={unit}", *keep, "--", *cmd],
                          capture_output=True, text=True, env=env)


def build_state(jid):
    """A proof build's state, as lifecycle says a job's: running while its
    unit is; done once it built all it could, each check's verdict then in
    whether its outputs are valid (proofs.verdicts); failed when it stopped
    short of that (stopped, killed, or nix couldn't build at all), timed out
    past proofTimeout; None when systemd can't say."""
    import common as C

    if os.path.exists(os.path.join(C.STATE, "builds", jid, "policy-paused")):
        return "parked"
    end = C.load(f"builds/{jid}/end.json", None)
    if end is not None:
        return end["state"]
    p = C.user_systemctl("show", "-P", "ActiveState", build_unit(jid))
    if p.returncode:
        return None
    if p.stdout.strip() in ("active", "activating", "deactivating", "reloading", "refreshing"):
        return "running"
    # Gone, with no end: unless it wrote one just after the first look.
    end = C.load(f"builds/{jid}/end.json", None)
    return end["state"] if end else "failed"
