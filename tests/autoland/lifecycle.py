"""Focused real Python lifecycle reader and handoff regression tests.
Run: python3 tests/autoland/lifecycle.py
"""
import fcntl
import json
import os
from pathlib import Path
import sys
import tempfile
import threading

sys.path.insert(0, sys.argv[1] if len(sys.argv) > 1 else
                str(Path(__file__).resolve().parents[2] / 'modules/autoland'))
with tempfile.TemporaryDirectory() as tmp:
    cfg = Path(tmp) / 'config.json'
    cfg.write_text(json.dumps(dict(context='test', stateDir=tmp, jobsDir=tmp,
                                  conary='cn', trainContext='train', snapDir=tmp)))
    os.environ['AUTOLAND_CONFIG'] = str(cfg)
    import common as C
    from step_jobs import Jobs
    from step_context import ON

    class Reader(Jobs):
        def __init__(self, kind, head):
            self.head = head
            self.tag = 'sam/r#5'
            self.st = {kind: 'original', kind + 'Job': 'job'}
            self.looked, self.ends, self.awaits = {}, {}, {}
        def deadline(self, kind):
            pass
        def breaking(self, kind):
            pass

    job = Path(tmp) / 'job'
    job.mkdir()
    (job / 'lock').touch()
    for kind in ON:
        for head in ('original', 'own-push'):
            for turn in (1, 2, 3):
                (job / 'turns').write_text(str(turn))
                (job / 'await').write_text('sam/up#3\t0\t{}\n')
                attempted = threading.Event()
                errors = []
                def resume():
                    with (job / 'lock').open('a') as lock:
                        try:
                            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                        except BlockingIOError:
                            pass
                        else:
                            errors.append('resume was not excluded')
                        attempted.set()
                        fcntl.flock(lock, fcntl.LOCK_EX)
                        (job / 'turns').write_text(str(turn + 1))
                        (job / 'await').unlink()
                threads = []
                def state(jid):
                    t = threading.Thread(target=resume)
                    threads.append(t)
                    t.start()
                    assert attempted.wait(5)
                    return 'done'
                C.job_state = state
                reader = Reader(kind, head)
                assert reader.fixing() == kind
                assert reader.awaits == {kind: 'sam/up#3'}
                assert not reader.ends
                threads[0].join(5)
                assert not threads[0].is_alive() and not errors, errors
                for live in ('queued', 'running'):
                    C.job_state = lambda jid: live
                    assert Reader(kind, head).fixing() == kind
            C.job_state = lambda jid: 'done'
            for targets in ('', 'SAM/R#5\t0\t{}\n'):
                (job / 'await').write_text(targets)
                reader = Reader(kind, head)
                assert reader.fixing() is None
                assert reader.ends == {kind: 'done'}
print('Python lifecycle and handoff: ok')
