"""The expedited step (expedite.py): a repo's pass told only PRs an author
put first steps those then and there, leaving the repo's other PRs, their
wake-ups, digest lines, waits and batch cursor as they were; anything else
told, or nothing, is the ordinary pass. A pass running yields to a first
PR's waiting, a continuation goes after it, and a pass that can't have the
lock puts back what it was told."""
from contextlib import ExitStack, contextmanager
import copy
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
from unittest.mock import patch

sys.path.insert(0, sys.argv[1] if len(sys.argv) > 1 else
                str(Path(__file__).resolve().parents[2] / 'modules/autoland'))
with tempfile.TemporaryDirectory() as tmp:
    state = Path(tmp) / 'state'
    marker = Path(tmp) / 'run' / '.first'
    told_file = Path(tmp) / 'told'
    cfg = Path(tmp) / 'config.json'
    cfg.write_text(json.dumps(dict(
        context='test', stateDir=str(state), snapDir=str(Path(tmp) / 'trees'),
        conary={'cn': 'cn', 'env': {}}, trainContext='train', baselineJobs=False,
        labels={'first': 'first', 'waiting': {}}, kick='http://events/kick/autoland',
        continuationKick='http://events/kick/autoland-continue', firstMarker=str(marker),
        watch={'kick': 'watchdog', 'escalated': f'{tmp}/escalated.json'},
        defaults={'stallAfter': 120}, repos={},
    )))
    os.environ['AUTOLAND_CONFIG'] = str(cfg)
    import common as C
    import expedite as X
    import steward as S

    def head(n):
        return f'{n:x}'.rjust(40, 'c')

    def pr(n, labels=()):
        return dict(number=n, id=f'PR_{n}', title=str(n), headRefOid=head(n), headRefName=f'branch-{n}',
                    baseRefName='main', isCrossRepository=False, labels={'nodes': [{'name': x} for x in labels]},
                    closingIssuesReferences={'nodes': []}, url=f'https://github.com/sam/r/pull/{n}')

    def line(n, what='old'):
        return ['Open over a day', f'[sam/r#{n}](https://github.com/sam/r/pull/{n}) {n}', what]

    REPO_LINE = ['A reviewer is out of service', 'Codex', 'spent']

    @contextmanager
    def fixture(firsts=(4,), step_wake=100):
        """sam/r with #4, labelled first (by an author, as the steward's
        firsts reads it, unless `firsts` says otherwise: tests/autoland/
        expedited.py has who counts), and #9 and #10, ordinary;
        a batch under way over it, owed and booked to continue; a wake-up,
        digest lines and waits from earlier passes. Each step records the PR
        and whether it went first, books a wake-up step_wake on, and lists a
        line of its own."""
        if state.exists():
            shutil.rmtree(state)
        state.mkdir()
        if marker.exists():
            marker.unlink()
        C.WAKE.clear()
        C.LIMITED.clear()
        C.NOW = 1000
        handled = []
        repos = [{'repo': 'sam/r', 'default': 'main'}]
        prs = [pr(4, ['first']), pr(9), pr(10)]
        info = {'defaultBranchRef': {'name': 'main', 'target': {'oid': 'base'}}}
        C.save('wake.json', {'sam/r': C.NOW + 500, 'sam/q': C.NOW + 50})
        C.save('digest-repos.json', {'sam/r': [line(4), line(9), REPO_LINE]})
        item = dict(repo='sam/r', what='w', link='l', phase='p', since=10, due=7210, kick='autoland/sam/r', claude=False, rev='x')
        C.save('watch.json', {'at': 10, 'items': {'sam/r#10': {**item, 'rev': f'{head(10)} main'},
                                                  'sam/r#4': {**item, 'rev': f'{head(4)} main'}}})
        C.save('pass-progress.json', {'repos': {'sam/r': {'done': {'9': 'old'}, 'visits': {'9': 2}, 'sequence': 2,
                                                          'digest': [line(9)], 'wake': C.NOW + 700}},
                                      'owed': ['sam/r'], 'finished': []})
        C.save('continue.json', {'sam/r': C.NOW + 1})

        def handle(repo, p, info, mine, held, under, first=False):
            handled.append((p['number'], first))
            mine.append(tuple(line(p['number'], 'new')))
            C.wake(repo, C.NOW + step_wake)

        with ExitStack() as stack:
            for obj, name, fn in (
                (S.R, 'discover', lambda: repos), (C, 'follow_renames', lambda *a: None),
                (S.R, 'restart', lambda *a: None), (S.R, 'settle_repo', lambda *a: False),
                (S.R, 'watch_default', lambda *a: {}), (S.R, 'publish', lambda *a: None),
                (S.R, 'hooks', lambda *a: None), (S, 'open_prs', lambda *a: (info, copy.deepcopy(prs))),
                (S.P, 'begin', lambda *a: None), (S.P, 'place', lambda repo, n: n),
                (S.T, 'begin', lambda repo: S.T.STOPPED.pop(repo, None)),
                (S.T, 'on', lambda *a: False), (S.T, 'step', lambda *a: None), (S.T, 'seat', lambda *a: None),
                (S.step, 'handle', handle), (S, 'branches', lambda *a, **k: None),
                (S, 'upkeep', lambda *a: None), (S.V, 'sweep', lambda *a: None),
                (C, 'taken', lambda *a: None), (C, 'poke', lambda *a: None),
                (S, 'firsts', lambda repo, prs: {p['number'] for p in prs} & set(firsts)),
                (C, 'coverage', lambda rs, done: {'repos': {r['repo']: {'covered': r in done} for r in rs}}),
            ):
                stack.enter_context(patch.object(obj, name, fn))
            yield handled, prs

    def kicked(told=None):
        """The repo's pass, as autoland@ runs it, told these lines (None:
        not told)."""
        env = {}
        if told is not None:
            told_file.write_text(told)
            env['BURROW_EVENTS_PRS'] = str(told_file)
        with patch.dict(os.environ, env):
            S.run_pass(['sam/r'])

    # Told only of the first PR, by its number, its branch or its head, or
    # all three, twice over: it alone is stepped, once, as a first PR. The
    # other PR's wake-up, digest line, wait and place in the batch stay; the
    # batch still owes the repo and still continues; its own digest line is
    # the step's.
    for told in ('pr 4\n', 'branch branch-4\n', f'commit {head(4)}\n',
                 f'pr 4\nbranch branch-4\ncommit {head(4)}\npr 4\n'):
        with fixture() as (handled, prs):
            kicked(told)
            assert handled == [(4, True)], (told, handled)
            assert C.load('wake.json', {}) == {'sam/r': C.NOW + 100, 'sam/q': C.NOW + 50}, C.load('wake.json', {})
            got = C.load('digest-repos.json', {})['sam/r']
            assert got == [line(9), REPO_LINE, line(4, 'new')], got
            items = C.load('watch.json', {})['items']
            assert 'sam/r#10' in items and items['sam/r#10']['since'] == 10, items
            progress = C.load('pass-progress.json', {})
            st = progress['repos']['sam/r']
            assert progress['owed'] == ['sam/r'] and st['done']['9'] == 'old' and '4' in st['done'], progress
            assert st['wake'] == C.NOW + 100 and st['digest'] == [line(9), line(4, 'new')], st
            assert C.load('continue.json', {}) == {'sam/r': C.NOW + 1}, C.load('continue.json', {})
            assert C.load(X.FILE, {}) == {'sam/r': [{'number': 4, 'branch': 'branch-4', 'head': head(4)}]}
    # Its step's wake-up later than the other PR's: theirs stands.
    with fixture(step_wake=900) as (handled, prs):
        kicked('pr 4\n')
        assert C.load('wake.json', {})['sam/r'] == C.NOW + 500, C.load('wake.json', {})
        assert C.load('pass-progress.json', {})['repos']['sam/r']['wake'] == C.NOW + 700
    # A repo with no batch under way gets no cursor from it.
    with fixture() as (handled, prs):
        C.save('pass-progress.json', {'repos': {}, 'owed': [], 'finished': []})
        kicked('pr 4\n')
        assert handled == [(4, True)] and C.load('pass-progress.json', {})['repos'] == {}
    print('a first PR alone, its repo left as it was: passed')

    # Told of anything else (an ordinary PR, with the first one or not, the
    # default branch, a PR not open, a commit that's no head), or nothing:
    # the ordinary pass, every PR, the first one first.
    for told in ('pr 9\n', 'pr 4\npr 9\n', 'branch main\n', 'pr 77\n', 'commit 0123456\n', 'nonsense\n', '', None):
        with fixture() as (handled, prs):
            kicked(told)
            assert handled[0] == (4, True) and sorted(handled[1:]) == [(9, False), (10, False)], (told, handled)
            assert 'sam/r' not in C.load('pass-progress.json', {})['owed'], told
    # A first PR's step as the ordinary pass takes it is a first PR's too.
    print('anything else told is the ordinary pass: passed')

    # One the steward's firsts doesn't name (labelled first by someone whose
    # word doesn't count) is the ordinary pass's.
    with fixture(firsts=()) as (handled, prs):
        kicked('pr 4\n')
        assert sorted(handled) == [(4, False), (9, False), (10, False)], handled
        assert C.load(X.FILE, {}) == {}, C.load(X.FILE, {})
    print('only what firsts names is expedited: passed')

    # A red default branch's held fixes are stepped with it, as every pass
    # steps them first; a PR it seats in its train books the ordinary pass
    # that steps the train.
    with fixture() as (handled, prs):
        seats = {}
        prs.append(pr(12))

        def step_and_seat(repo, p, info, mine, held, under, first=False):
            handled.append((p['number'], first, held))
            seats[p['number']] = 'in'

        with patch.object(S.R, 'watch_default', lambda *a: {12: 'Waiting for CI on main'}), \
                patch.object(S.step, 'handle', step_and_seat), \
                patch.object(S.T, 'seat', lambda repo, n: seats.get(n)):
            kicked('pr 4\n')
        assert handled == [(4, True, None), (12, False, 'Waiting for CI on main')], handled
        assert C.load('wake.json', {})['sam/r'] == C.NOW + 1, C.load('wake.json', {})
    # One whose step fails, or the repo's watch failing, ends nothing of the
    # batch it found.
    for failing in ('step', 'watch'):
        with fixture() as (handled, prs):
            def boom(*a, **k):
                raise RuntimeError('GitHub said 502')
            with patch.object(S.step if failing == 'step' else S.R, 'handle' if failing == 'step' else 'watch_default', boom):
                kicked('pr 4\n')
            progress = C.load('pass-progress.json', {})
            assert progress['owed'] == ['sam/r'] and progress['repos']['sam/r']['done']['9'] == 'old', (failing, progress)
            assert C.load('continue.json', {}) == {'sam/r': C.NOW + 1}, failing
            if failing == 'step':
                assert progress['repos']['sam/r']['unseen'] == ['4'], progress
    print('held fixes, trains and failures: passed')

    # A pass that can't have the lock (a continuation has it) puts itself
    # back told what it was, so the first PR's stays first.
    with fixture():
        kicks = []
        told_file.write_text('pr 4\nbranch branch-4\n')
        with patch.dict(os.environ, {'BURROW_EVENTS_PRS': str(told_file)}), \
                patch.object(sys, 'argv', ['steward', 'kick', 'sam/r']), \
                patch.object(S.fcntl, 'flock', side_effect=BlockingIOError), \
                patch.object(C, 'poke', lambda url, body=b'': kicks.append((url, body))):
            S.main()
        assert kicks == [('http://events/kick/autoland/sam/r', b'branch branch-4\npr 4\n')], kicks
    # A continuation that finds a first PR's pass waiting goes after it,
    # without a step; one left over from long ago is no wait.
    for age, waits in ((0, True), (X.MARKER_FRESH + 60, False)):
        with fixture():
            marker.parent.mkdir(exist_ok=True)
            marker.write_text('')
            old = C.time.time() - age
            os.utime(marker, (old, old))
            kicks, ran = [], []
            with patch.object(sys, 'argv', ['steward', 'continue', 'sam/r']), \
                    patch.object(S.fcntl, 'flock', lambda *a: None), \
                    patch.object(S, 'run_pass', lambda *a: ran.append(a)), \
                    patch.object(C, 'poke', lambda url, body=b'': kicks.append(url)):
                S.main()
            assert (kicks, ran) == ((['http://events/kick/autoland-continue/sam/r'], []) if waits
                                    else ([], [(['sam/r'], True)])), (age, kicks, ran)
    # A pass under way yields at its next PR to a first PR's pass waiting,
    # saving its cursor; its continuation, once that's run, does the rest.
    with fixture(firsts=()) as (handled, prs):
        C.save('pass-progress.json', {'repos': {}, 'owed': [], 'finished': []})
        C.save('continue.json', {})
        original = S.step.handle

        def and_then_an_event(*a, **k):
            original(*a, **k)
            marker.parent.mkdir(exist_ok=True)
            marker.write_text('')

        with patch.object(S.step, 'handle', and_then_an_event):
            kicked('pr 9\n')
        assert handled == [(4, False)], handled
        assert 'sam/r' in C.load('continue.json', {}) and C.load('pass-progress.json', {})['repos']['sam/r'], \
            C.load('pass-progress.json', {})
        marker.unlink()
        S.run_pass(['sam/r'], continuing=True)
        assert handled == [(4, False), (9, False), (10, False)], handled
    print('a running pass yields, a continuation waits, a busy one keeps what it was told: passed')
