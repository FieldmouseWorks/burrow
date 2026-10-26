"""A verdict's seal: a MAC under a key kept in a state dir, which only the
job a verdict is asked of is given a way to make. Every Claude job posts as
the same login, so without it a job quoting PR text that holds a
well-formed marker would pass for the verdict. Shared by the autoland
steward's judges (common.py) and renovate-sweep's verdicts on
majors (../updates/major-seal.py), each with a key of its own."""

import hashlib
import hmac
import os
import re

KEY_RE = re.compile(r"^[0-9a-f]{64}$")
# renovate-sweep's verdict on a Renovate major.
MAJOR = "major-verdict"


def key(path, create=True):
    """The key at path; with create, a new one if it's missing or isn't one
    (else None). An empty or short key (a crash or a full disk as it was
    made) is one anyone could seal with, so a new one, whole or not at all.
    Verdicts sealed with a damaged one stop counting."""
    try:
        k = open(path).read().strip()
    except FileNotFoundError:
        k = None
    if KEY_RE.match(k or ""):
        return k
    if not create:
        return None
    if k is not None:
        print(f"{path} isn't a key; making a new one", flush=True)
    tmp = f"{path}.{os.getpid()}.new"
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        f.write(os.urandom(32).hex())
        f.flush()
        os.fsync(f.fileno())
    if k is None:
        # Linked in whole, and only if no one made one meanwhile.
        try:
            os.link(tmp, path)
        except FileExistsError:
            pass
        os.remove(tmp)
    else:
        os.replace(tmp, path)
    k = open(path).read().strip()
    if not KEY_RE.match(k):
        raise RuntimeError(f"{path} couldn't be written")
    return k


def mac(k, repo, n, name, verdict, head, base):
    """What a marker carries after the head. It binds the base too: a PR
    retargeted onto another branch has another diff, which its verdict
    wasn't about."""
    msg = f"{repo}#{n} {name} {verdict} {head} {base}".encode()
    return hmac.new(k.encode(), msg, hashlib.sha256).hexdigest()[:32]


def major(k, repo, n, verdict, head, base):
    """A major's seal. The repo in lower case: the sweep reads it from a
    clone's remote, the steward as GitHub spells it, and GitHub doesn't
    mind case."""
    return mac(k, repo.lower(), n, MAJOR, verdict, head, base)
