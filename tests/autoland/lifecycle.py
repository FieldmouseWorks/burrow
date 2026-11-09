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

    def holds(jid):
        """`proj holds`, as proj reads the job's record: its awaits, and a
        question while QUESTION has one."""
        lines = [] if not (job / 'await').exists() else \
            [f"await {l.split(chr(9))[0]}" for l in (job / 'await').read_text().splitlines()]
        return QUESTION + lines
    QUESTION = []
    C.holds = holds
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
    # A job of any kind holding a question (or a one-way one left for the
    # owner) is still at work, however its turn ended: never lost, never
    # handed again (remi#881).
    C.job_state = lambda jid: 'done'
    (job / 'await').write_text('SAM/R#5\t0\t{}\n')
    for held, said in (('ask', 'an answer to its question'), ('owner', "Sam's answer to its one-way question")):
        QUESTION[:] = [held]
        C.CFG['owner'] = 'Sam'
        for kind in (*ON, 'judged', 'proved'):
            reader = Reader(kind, 'original')
            assert reader.running(kind, 'original') == 'awaiting', (held, kind)
            assert reader.awaits == {kind: said} and not reader.ends, (held, kind, reader.awaits)
            assert C.at_work('job') and C.at_work('job', 'sam/r#5'), held
    QUESTION[:] = []
    for kind in (*ON, 'judged', 'proved'):
        reader = Reader(kind, 'original')
        assert reader.running(kind, 'original') == '' and reader.ends == {kind: 'done'}, kind
    assert not C.at_work('job', 'sam/r#5') and C.at_work('job'), 'awaiting only its own PR is ending its turn'
    # proj that can't say what it holds keeps the job, as one it can't say the state of.
    def fails(jid):
        raise RuntimeError('proj holds job: exit 1')
    C.holds = fails
    assert C.job_lifecycle('job', 'sam/r#5') == (None, None) and C.at_work('job', 'sam/r#5')
print('Python lifecycle and handoff: ok')
