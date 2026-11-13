"""Focused real Python lifecycle reader and handoff regression tests: what
the steward takes a job's lifecycle (`proj lifecycle`, which reads its state
and what it holds under the job's turn-start lock: crates/burrow-proj's
lifecycle_reads_under_the_turn_start_lock) to say, for every kind of job.
Run: python3 tests/autoland/lifecycle.py
"""
import json
import os
from pathlib import Path
import sys
import subprocess
import tempfile
from unittest.mock import patch

sys.path.insert(0, sys.argv[1] if len(sys.argv) > 1 else
                str(Path(__file__).resolve().parents[2] / 'modules/autoland'))
# Keep deadline coverage on the established test entry point, including
# when the negative gate runs these tests with the base check definition.
subprocess.run([sys.executable, str(Path(__file__).with_name("timeouts.py")),
                *sys.argv[1:]], check=True)

subprocess.run([sys.executable, str(Path(__file__).with_name("batches.py")),
                *sys.argv[1:]], check=True)

subprocess.run([sys.executable, str(Path(__file__).with_name("continuations.py")),
                *sys.argv[1:]], check=True)

with tempfile.TemporaryDirectory() as tmp:
    cfg = Path(tmp) / 'config.json'
    cfg.write_text(json.dumps(dict(context='test', stateDir=tmp, jobsDir=tmp, owner='Sam', proj=f'{tmp}/proj',
                                  conary='cn', trainContext='train', snapDir=tmp)))
    os.environ['AUTOLAND_CONFIG'] = str(cfg)
    import common as C
    from step_jobs import Jobs
    from step_context import ON

    KINDS = (*ON, 'judged', 'proved')

    class Reader(Jobs):
        def __init__(self, kind, head):
            self.head = head
            self.tag = 'sam/r#5'
            self.st = {kind: 'original', kind + 'Job': 'job'}
            self.looked, self.ends, self.awaits = {}, {}, {}
            self.refresh = {}
            self.repo = "sam/r"
        def deadline(self, kind):
            pass
        def breaking(self, kind):
            pass

    def says(state, *held):
        C.lifecycle = lambda jid: (state, list(held))

    # A fix job awaiting upstream is still at work, on the head it was handed
    # or its own push, until proj resumes it; queued or running, it is.
    for kind in ON:
        for head in ('original', 'own-push'):
            says('done', 'await sam/r#5', 'await sam/up#3')
            reader = Reader(kind, head)
            assert reader.fixing() == kind
            assert reader.awaits == {kind: 'sam/up#3'} and not reader.ends
            for live in ('queued', 'running', None):
                says(live)
                assert Reader(kind, head).fixing() == kind
            for held in ((), ('await SAM/R#5',)):
                says('done', *held)
                reader = Reader(kind, head)
                assert reader.fixing() is None
                assert reader.ends == {kind: 'done'}

    # A fix job between turns on its own PR whose turn's answers just went
    # up (claims.rs's owns) is still at work: what went up resumes it, so
    # no second job is handed its branch (PSIV#459). One whose turn left
    # nothing new has ended, as above; and a judge or proof job awaiting its
    # own PR ended whatever claims says.
    for kind in ON:
        for head in ('original', 'own-push'):
            for resumes, at in ((True, kind), (False, None)):
                says('done', 'await SAM/R#5')
                reader = Reader(kind, head)
                reader.st['answering'] = {'job': {'kind': kind, 'waits': True, 'owns': resumes}}
                assert reader.fixing() == at, (kind, head, resumes)
                if resumes:
                    assert reader.awaits == {kind: "this PR's next change"} and not reader.ends
                else:
                    assert reader.ends == {kind: 'done'}
    for kind in ('judged', 'proved'):
        says('done', 'await sam/r#5')
        reader = Reader(kind, 'original')
        reader.st['answering'] = {'job': {'kind': 'handed', 'waits': True, 'owns': True}}
        assert reader.running(kind, 'original') == '' and reader.ends == {kind: 'done'}

    # A job of any kind holding a question (or a one-way one left for the
    # owner) is still at work, however its turn ended (a failed resume
    # leaves its question, whose deadline resumes it): never lost, never
    # handed again (remi#881). So is one awaiting anything after a failed
    # turn. A turn that broke holding nothing is how it ended.
    for state in ('done', 'failed', 'error', 'timed out after 60m'):
        for held, said in (('ask', 'an answer to its question'), ('owner', "Sam's answer to its one-way question"),
                           ('await sam/up#3', 'sam/up#3')):
            says(state, 'await sam/r#5', held)
            for kind in KINDS:
                reader = Reader(kind, 'original')
                assert reader.running(kind, 'original') == 'awaiting', (state, held, kind)
                assert reader.awaits == {kind: said} and not reader.ends, (state, held, kind, reader.awaits)
            assert C.at_work('job') and C.at_work('job', 'sam/r#5'), (state, held)
        says(state, 'await sam/r#5')
        for kind in KINDS:
            reader = Reader(kind, 'original')
            assert reader.running(kind, 'original') == '' and reader.ends == {kind: state}, (state, kind)
        assert not C.at_work('job', 'sam/r#5') and C.at_work('job'), 'awaiting only its own PR is ending its turn'

    # Usage is a hold of the selected engine, for every kind of step.
    for engine in ('Claude', 'Codex'):
        for kind in KINDS:
            reset = int(C.NOW) + 3600
            says('failed', f'await usage-limit', f'usage {engine} {reset}')
            reader = Reader(kind, 'original')
            assert reader.running(kind, 'original') == 'usage'
            assert reader.st['usageJobs']['job']['release'] == reset and not reader.ends
            assert C.at_work('job')
            assert C.WAKE['sam/r'] == reset

    # Early release is scoped to the job that was held. Other live holds
    # survive, and an ended or replaced job cannot defer unrelated work.
    for engine in ('Claude', 'Codex'):
        says('queued', f'usage {engine} {int(C.NOW) + 7200}')
        reader = Reader('judged', 'original')
        reader.running('judged', 'original')
        release = int(C.NOW) - 30
        says('running', f'released {engine} {release}')
        reader.looked, reader.refresh = {}, {}
        assert reader.running('judged', 'original') == 'running'
        assert reader.usage_grace() == release
        says('queued', f'usage {engine} {int(C.NOW) + 3600}')
        reader.st.update(proved='original', provedJob='second')
        reader.running('proved', 'original')
        assert reader.usage_grace() == int(C.NOW) + 3600
        says('failed')
        reader.looked, reader.refresh = {}, {}
        reader.running('judged', 'original')
        assert 'job' not in reader.st['usageJobs'] and reader.usage_grace() is None
        reader.st['judgedJob'] = 'replacement'
        says('running')
        reader.running('judged', 'original')
        assert reader.usage_grace() is None

    # A failed read preserves the known hold for every job kind/engine.
    # Grace is computed after cancellation/replacement, not accumulated
    # during earlier reads in the same pass.
    for engine in ('Claude', 'Codex'):
        for kind in KINDS:
            reset = int(C.NOW) + 3600
            says('queued', f'usage {engine} {reset}')
            reader = Reader(kind, 'original')
            reader.running(kind, 'original')
            says(None)
            reader.looked.clear()
            C.WAKE.clear()
            assert reader.running(kind, 'original') == 'usage'
            assert reader.usage_grace() == reset
            assert C.NOW < C.WAKE['sam/r'] <= min(reset, C.NOW + 60)
            reader.st.update(other='original', otherJob='other-job')
            says('queued', f'usage {engine} {reset - 100}')
            reader.running('other', 'original')
            reader.st.pop(kind + 'Job')
            assert reader.usage_grace() == reset - 100
            reader.st.pop('otherJob')
            assert reader.usage_grace() is None
            reader.st[kind + 'Job'] = 'replacement'
            says('queued')
            reader.running(kind, 'original')
            assert reader.usage_grace() is None

    # Parked questions and proofs retain their records while holding no lane.
    says('parked', 'ask', 'await sam/up#3')
    assert not C.at_work('job')
    import proofs as P
    state = {'provedJob': 'job', 'proofsLive': ['job'], 'proofPaths': {'job': {'paths': ['kept']}}}
    assert P.live(state) == []
    assert state['proofsLive'] == ['job'] and state['proofPaths']['job']['paths'] == ['kept']
    folder = Path(tmp) / 'builds' / 'proof-paused'
    folder.mkdir(parents=True)
    (folder / 'policy-paused').write_text('paused')
    (folder / 'build.json').write_text('{}')
    (folder / 'end.json').write_text('{"state":"parked"}')
    P.prune()
    assert folder.is_dir(), 'parked builds and their roots never age out'
    (folder / 'build.json').write_text('{"repo":"sam/r"}')
    (folder / 'policy-restoring').write_text('resuming')
    with patch.object(C, 'paused', return_value=True):
        P.run('proof-paused')
    assert (folder / 'policy-restoring').exists(), 'denied restoration must retain its retry marker'

    # What proj says: its state, then each thing held; one it can't read the
    # record of (it exits non-zero) is kept, as one nothing can say of.
    del C.lifecycle
    import importlib
    importlib.reload(C)
    proj = Path(tmp) / 'proj'
    for out, code, want in (('failed\nask\n', 0, ('failed', ['ask'])), ('done\n', 0, ('done', [])),
                            ('', 1, (None, [])), ('done\n', 1, (None, []))):
        proj.write_text(f"#!/bin/sh\nprintf '{out}'\nexit {code}\n")
        proj.chmod(0o755)
        assert C.lifecycle('job') == want, (out, code, C.lifecycle('job'))
    assert C.at_work('job') is True, 'unknown is kept'
# A retirement look that posts answers, or leaves them to go again, hands the
# branch to no new fix job though no job owns it: the PR it read is stale.
import claims
for status, clear, want in ((None, True, True), (None, False, False),
                            ('A Claude job\'s answers went up: reading the PR again', True, False),
                            ("GitHub didn't take a Claude job's answers: they go up next try", True, False)):
    with patch.object(claims, 'see', lambda *a, **k: (status, {'clear': clear})):
        assert claims.stand_down('r', {}, {}, 't', None, None, None) is want, (status, clear)
with patch.object(claims, 'see', lambda *a, **k: (None, None)):
    assert claims.stand_down('r', {}, {}, 't', None, None, None) is True
print('Python lifecycle and handoff: ok')
