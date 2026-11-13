"""Continuation receipts never acknowledge new work or deferred failures."""
from contextlib import ExitStack, contextmanager
import copy
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from unittest.mock import patch

sys.path.insert(0, sys.argv[1] if len(sys.argv) > 1 else
                str(Path(__file__).resolve().parents[2] / 'modules/autoland'))
with tempfile.TemporaryDirectory() as tmp:
    state = Path(tmp) / 'state'
    cfg = Path(tmp) / 'config.json'
    cfg.write_text(json.dumps(dict(
        context='test', stateDir=str(state), snapDir=str(Path(tmp) / 'trees'),
        conary={'cn': 'cn', 'env': {}}, trainContext='train', baselineJobs=False,
        labels={'first': 'first', 'waiting': {}}, kick='http://events/kick/autoland',
        continuationKick='http://events/kick/autoland-continue',
        watch={'kick': 'watchdog', 'escalated': f'{tmp}/escalated.json'},
        defaults={'stallAfter': 120}, repos={},
    )))
    os.environ['AUTOLAND_CONFIG'] = str(cfg)
    import common as C
    import steward as S
    import waits as W

    def pr(n):
        return dict(number=n, title=str(n), headRefOid=f'head-{n}', headRefName=f'branch-{n}',
                    baseRefName='main', labels={'nodes': []}, url=f'https://github.com/sam/r/pull/{n}')

    @contextmanager
    def fixture():
        if state.exists():
            shutil.rmtree(state)
        state.mkdir()
        C.WAKE.clear()
        C.LIMITED.clear()
        C.NOW = 1000
        clock, handled, facts = [0], [], {'local': 'before'}
        repos = [{'repo': 'sam/r', 'default': 'main'}]
        prs = [pr(1), pr(2)]
        info = {'defaultBranchRef': {'name': 'main', 'target': {'oid': 'base'}}}

        def handle(repo, p, info, mine, held, under):
            handled.append((p['number'], facts['local'], under))
            clock[0] += 1

        with ExitStack() as stack:
            for obj, name, fn in (
                (S.R, 'discover', lambda: repos), (C, 'follow_renames', lambda *a: None),
                (S.R, 'restart', lambda *a: None), (S.R, 'settle_repo', lambda *a: False),
                (S.R, 'watch_default', lambda *a: {}), (S.R, 'publish', lambda *a: None),
                (S.R, 'hooks', lambda *a: None), (S, 'open_prs', lambda *a: (info, copy.deepcopy(prs))),
                (S.P, 'begin', lambda *a: None), (S.P, 'place', lambda repo, n: n),
                (S.T, 'begin', lambda repo: S.T.STOPPED.pop(repo, None)),
                (S.T, 'on', lambda *a: True), (S.T, 'step', lambda *a: None),
                (S.step, 'handle', handle), (S, 'branches', lambda *a, **k: None),
                (S, 'upkeep', lambda *a: None), (S.V, 'sweep', lambda *a: None),
                (C, 'taken', lambda *a: None), (C, 'poke', lambda *a: None),
                (C, 'coverage', lambda rs, done: {'repos': {r['repo']: {'covered': r in done} for r in rs}}),
            ):
                stack.enter_context(patch.object(obj, name, fn))
            stack.enter_context(patch('time.monotonic', side_effect=lambda: clock[0]))
            yield clock, handled, facts, repos, prs, info

    # Every preparation path may consume the soft budget. One PR attempt
    # still reaches the persisted cursor, in both full and kicked passes.
    stages = [(S.R, 'discover'), (C, 'follow_renames'), (S.R, 'restart'), (S.R, 'settle_repo'),
              (S, 'open_prs'), (S.T, 'begin'), (S.R, 'watch_default')]
    for kick in (None, ['sam/r']):
        for obj, name in stages:
            if kick and name == 'follow_renames':
                continue  # rename retirement is full-pass work
            with fixture() as (clock, handled, facts, repos, prs, info):
                original = getattr(obj, name)

                def slow(*args, **kwargs):
                    clock[0] += 9 * 60
                    return original(*args, **kwargs)

                with patch.object(obj, name, slow):
                    S.run_pass(kick)
                assert handled == [(1, 'before', None), (2, 'before', None)], (kick, name, handled)
                progress = C.load('pass-progress.json', {})
                assert progress['repos'] == {}, (kick, name, progress)
                assert (state / 'watch.json').exists(), (kick, name)
    print('slow preparation makes durable progress: passed')

    # A normal full pass consumes repository events in Line.take. Finished
    # and unvisited repos, including expired retries, must get ordinary work
    # requests back, separate from the internal continuation line.
    with fixture() as (clock, handled, facts, repos, prs, info):
        repos[:] = [{'repo': f'sam/{n}', 'default': 'main'} for n in 'arb']
        C.save('pass-progress.json', {'full': True, 'finished': ['sam/a'], 'repos': {}})
        C.save('wake.json', {'sam/a': C.NOW - 50, 'sam/b': C.NOW - 40, '': C.NOW - 30})
        original = S.step.handle

        def long(*args):
            original(*args)
            clock[0] += 9 * 60

        with patch.object(S.step, 'handle', long):
            S.run_pass(None)
        wakes = C.load('wake.json', {})
        assert wakes['sam/a'] > C.NOW and wakes['sam/b'] > C.NOW, wakes
        assert '' not in wakes, wakes
        assert '' in C.load('continue.json', {}), C.load('continue.json', {})
    print('full passes preserve skipped event and deadline obligations: passed')

    # Local completions/availability, lifecycle/held answers and sibling PR
    # changes all arrive on the ordinary line. Invalidate decisions on that
    # line while keeping visited ordering; an internal continuation keeps
    # receipts and eventually consumes the new facts without another event.
    for source in ('review done', 'reviewer availability', 'proof done', 'proof slot',
                   'job lifecycle', 'held answer', 'stacking relationship'):
        with fixture() as (clock, handled, facts, repos, prs, info):
            from batch import Batch
            batch = Batch(['sam/r'], continuing=True)
            batch.note('sam/r', prs[0], [], True, info)
            batch.end(False, ['sam/r'])
            facts['local'] = source
            if source == 'stacking relationship':
                prs[1]['headRefName'] = 'main'
            original = S.step.handle

            def long(*args):
                original(*args)
                clock[0] += 9 * 60

            with patch.object(S.step, 'handle', long):
                S.run_pass(['sam/r'])
            assert handled[0][:2] == (2, source), (source, handled)
            S.run_pass(['sam/r'], continuing=True)
            assert [n for n, _, _ in handled] == [2, 1], (source, handled)
            assert handled[-1][1] == source, handled
            if source == 'stacking relationship':
                assert handled[-1][2] == 2, handled
    # A stream of new-work kicks must rotate old receipts too, rather than
    # repeatedly selecting the same first PR after every invalidation.
    with fixture() as (clock, handled, facts, repos, prs, info):
        prs.append(pr(3))
        batch = Batch(['sam/r'], continuing=True)
        for p in prs:
            batch.note('sam/r', p, [], True, info)
        batch.end(False, ['sam/r'])
        original = S.step.handle

        def long(*args):
            original(*args)
            clock[0] += 9 * 60

        with patch.object(S.step, 'handle', long):
            for _ in range(3):
                S.run_pass(['sam/r'])
        assert [n for n, _, _ in handled] == [1, 2, 3], handled
        S.run_pass(['sam/r'], continuing=True)
        assert C.load('pass-progress.json', {})['repos'] == {}
    # Startup catch-up on the continuation line consumes its per-repo
    # requests too. A partial kicked traversal must still run when there
    # is no active fleet-wide traversal, without processing unrelated repos.
    with fixture() as (clock, handled, facts, repos, prs, info):
        repos.append({'repo': 'sam/q', 'default': 'main'})
        batch = Batch(['sam/r'], continuing=True)
        batch.note('sam/r', prs[0], [], True, info)
        batch.end(False, ['sam/r'])
        C.save('wake.json', {'sam/q': C.NOW - 1})
        S.run_pass(None, continuing=True)
        assert handled == [(2, 'before', None)], handled
        assert C.load('pass-progress.json', {})['repos'] == {}
        assert C.load('wake.json', {})['sam/q'] > C.NOW
    # Expired decision deadlines also invalidate an internal receipt once,
    # rather than perpetually carrying the expired time into the next batch.
    with fixture() as (clock, handled, facts, repos, prs, info):
        batch = Batch(['sam/r'], continuing=True)
        C.WAKE['sam/r'] = C.NOW - 1
        batch.note('sam/r', prs[0], [], True, info)
        batch.end(False, ['sam/r'])
        C.WAKE.clear()
        S.run_pass(['sam/r'], continuing=True)
        assert sorted(n for n, _, _ in handled) == [1, 2], handled
    print('new local facts and expired waits invalidate receipts: passed')

    # An old repository failure is retained while its operation is deferred;
    # a failed retry preserves its original deadline, and success clears it.
    for operation in ('train', 'train unavailable', 'branches', 'upkeep'):
        with fixture() as (clock, handled, facts, repos, prs, info):
            old = dict(repo='sam/r', what='sam/r', link='https://github.com/sam/r',
                       phase=W.FAILS, since=10, due=7210, kick='autoland/sam/r', claude=False, rev=None)
            C.save('watch.json', {'at': 10, 'items': {'sam/r': old}})
            original = S.step.handle

            def long(*args):
                original(*args)
                clock[0] += 9 * 60

            with patch.object(S.step, 'handle', long):
                S.run_pass(None)
            assert C.load('watch.json', {})['items']['sam/r'] == old, operation

            def fail(*args, **kwargs):
                if operation == 'train unavailable':
                    raise C.Unavailable('temporarily unavailable')
                if operation == 'upkeep':
                    S.failing('sam/r', 'its upkeep failed')
                else:
                    raise RuntimeError('still failing')

            obj, name = (S.T, 'step') if operation.startswith('train') else (S, operation)
            with patch.object(obj, name, fail):
                C.NOW += 600
                S.run_pass(None, continuing=True)
            current = C.load('watch.json', {})['items']['sam/r']
            assert current['since'] == 10 and current['due'] == 7210, (operation, current)
            C.NOW += 600
            S.run_pass(['sam/r'])
            assert 'sam/r' not in C.load('watch.json', {})['items'], operation
    print('deferred operation failures keep their original watchdog clocks: passed')

    # Exhausting the work slice on the final PR still defers repository
    # operations; the continuation can run them without repeating the PR.
    with fixture() as (clock, handled, facts, repos, prs, info):
        prs[:] = prs[:1]
        operations = []
        original = S.step.handle

        def long(*args):
            original(*args)
            clock[0] += 9 * 60

        with patch.object(S.T, 'step', operations.append):
            with patch.object(S.step, 'handle', long):
                S.run_pass(['sam/r'])
            assert operations == [], operations
            S.run_pass(['sam/r'], continuing=True)
        assert operations == ['sam/r'] and len(handled) == 1, (operations, handled)

    # A repo busy enough that every pass is new work never finishes its
    # traversal in one slice. Its branch watch and upkeep still run each
    # batch, leaving every open PR's branches, the unstepped ones' too; only
    # the train waits for the whole traversal.
    with fixture() as (clock, handled, facts, repos, prs, info):
        prs[:] = [pr(n) for n in range(1, 6)]
        watched, kept, trained = [], [], []
        original = S.step.handle

        def long(*args):
            original(*args)
            clock[0] += 9 * 60

        def watch(repo, default, in_use, mine, full):
            watched.append((repo, default, set(in_use)))

        with patch.object(S, 'branches', watch), patch.object(S, 'upkeep', lambda repo, mine: kept.append(repo)), \
                patch.object(S.T, 'step', trained.append), patch.object(S.step, 'handle', long):
            for _ in range(3):
                S.run_pass(['sam/r'])
        assert len(handled) == 3 and trained == [], (handled, trained)
        everyone = {f'branch-{n}' for n in range(1, 6)} | {'main'}
        assert watched == [('sam/r', 'main', everyone)] * 3, watched
        assert kept == ['sam/r'] * 3, kept
    print('a busy repo still watches its branches every batch: passed')

    # A bounded multi-repo kick owes the repo it never entered, and an
    # unavailable train is retried at its own deadline, not at once.
    with fixture() as (clock, handled, facts, repos, prs, info):
        repos[:] = [{'repo': 'sam/r', 'default': 'main'}, {'repo': 'sam/s', 'default': 'main'}]
        original = S.step.handle

        def long(*args):
            original(*args)
            clock[0] += 9 * 60

        with patch.object(S.step, 'handle', long):
            S.run_pass(['sam/r', 'sam/s'])
        assert 'sam/s' in C.load('pass-progress.json', {})['owed'], C.load('pass-progress.json', {})
        for _ in range(4):
            S.run_pass(['sam/r', 'sam/s'], continuing=True)
        assert len(handled) >= 4, ('the repo the kick never entered was lost', handled)
        assert C.load('pass-progress.json', {}).get('owed') == [], C.load('pass-progress.json', {})

    with fixture() as (clock, handled, facts, repos, prs, info):
        def down(repo):
            raise C.Unavailable('temporarily unavailable')

        with patch.object(S.T, 'step', down):
            S.run_pass(['sam/r'])
        due = C.load('continue.json', {})['sam/r']
        assert due >= C.NOW + 5 * 60, (due, C.NOW)

    # A slow failed retry may already be due again when a batch finishes.
    # It must yield to ready repositories instead of repeatedly taking the
    # entire next slice at the head of the discovery order.
    with fixture() as (clock, handled, facts, repos, prs, info):
        repos.append({'repo': 'sam/s', 'default': 'main'})
        attempts = []

        def slow_retry(repo):
            attempts.append(repo)
            if repo == 'sam/r':
                clock[0] += 9 * 60
                raise C.Unavailable('slow failure')

        with patch.object(S.T, 'step', slow_retry):
            S.run_pass(None)
            assert attempts == ['sam/r'], attempts
            C.WAKE.clear()
            C.NOW += 600
            S.run_pass(None, continuing=True)
        assert attempts[:2] == ['sam/r', 'sam/s'], attempts

    with fixture() as (clock, *_):
        seen = []
        with C.pass_deadline(18 * 60):
            clock[0] += 17 * 60
            with patch.object(C.subprocess, 'run', side_effect=lambda *a, **k: seen.append(k['timeout']) or subprocess.CompletedProcess(a, 0)):
                C.remote(['command'], timeout=300)
            assert seen == [60], seen
            clock[0] += 60
            try:
                C.remote(['must-not-start'])
            except C.Unavailable:
                pass
            else:
                raise AssertionError('external work outlived the checkpoint reserve')
        assert C.REMOTE_DEADLINE is None
        # Upkeep subprocesses must share the same publication reserve as
        # GitHub calls; operation-only continuations cannot bypass it.
        clock[0] = 0
        with C.pass_deadline(18 * 60), patch.dict(C.CFG, {'upkeep': ['upkeep']}), patch.object(C, 'gh_env', return_value={}):
            clock[0] = 17 * 60
            for bounded in (True, False):
                with patch.object(C.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, '{"limited": null}')) as ran:
                    C.upkeep('branches', 'sam/r', bounded=bounded)
                assert ran.call_args.kwargs['timeout'] == 60, ran.call_args.kwargs

    # Contending units requeue on their own line, without a blocking lock or
    # rewriting wake files owned by the current pass.
    with fixture():
        kicks = []
        for argv, endpoint in ((['steward', 'kick', 'sam/r'], '/autoland/sam/r'),
                               (['steward', 'continue', 'sam/r'], '/autoland-continue/sam/r')):
            with patch.object(sys, 'argv', argv), patch.object(S.fcntl, 'flock', side_effect=BlockingIOError), patch.object(C, 'poke', kicks.append):
                S.main()
            assert kicks[-1].endswith(endpoint), kicks
        assert not (state / 'wake.json').exists()
        with patch.object(sys, 'argv', ['steward', 'continue', 'sam/r']), patch.object(S.fcntl, 'flock', side_effect=BlockingIOError), patch.object(C, 'poke', return_value=False):
            try:
                S.main()
            except SystemExit as error:
                assert error.code == 75
            else:
                raise AssertionError('unaccepted work was acknowledged')
    print('hard deadline reserve and nonblocking pass admission: passed')

    # Owed targets survive every entry point, even without a PR cursor.
    # Discovery and requests can spell the same GitHub repo differently.
    for route in ('per-repo', 'startup', 'ordinary full', 'ordinary kick'):
        with fixture() as (clock, handled, facts, repos, prs, info):
            repos[:] = [{'repo': 'Sam/R', 'default': 'main'}, {'repo': 'SAM/S', 'default': 'main'},
                        {'repo': 'sam/unrelated', 'default': 'main'}]
            C.save('pass-progress.json', {'owed': ['sAM/s'], 'repos': {}, 'finished': []})
            if route == 'per-repo':
                S.run_pass(['sam/s'], continuing=True)
            elif route == 'startup':
                S.run_pass(None, continuing=True)
            elif route == 'ordinary full':
                S.run_pass(None)
            else:
                S.run_pass(['sam/s'])
            assert len(handled) == (6 if route == 'ordinary full' else 2), (route, handled)
            assert C.load('pass-progress.json', {})['owed'] == [], route
            handled.clear()
            S.run_pass(['sam/s'], continuing=True)
            S.run_pass(None, continuing=True)
            assert handled == [], ('stale continuation', route, handled)

    # Real budget boundaries: a full catch-up consumes the requests for A
    # and B together, but must keep B through a second bounded batch.
    with fixture() as (clock, handled, facts, repos, prs, info):
        repos.append({'repo': 'Sam/S', 'default': 'main'})
        original = S.step.handle

        def slow_repo(*args):
            original(*args)
            clock[0] += 9 * 60

        with patch.object(S.step, 'handle', slow_repo):
            S.run_pass(['sam/r', 'sam/s'])
            C.WAKE.clear()
            S.run_pass(None, continuing=True)
        assert 'sam/s' in C.load('pass-progress.json', {})['owed']
        C.WAKE.clear()
        S.run_pass(None, continuing=True)
        assert len(handled) == 4, handled
        assert C.load('pass-progress.json', {})['owed'] == []
    print('owed targets survive full catch-up, case changes and stale requests: passed')

    # Stops belong to individual PR evaluations. Revalidating another PR
    # must not clear one; successfully retrying its cause must clear it,
    # even when an ordinary retry was consumed mid-traversal.
    for cause in ('withdrawal', 'exception'):
        for mid_traversal in (False, True):
            with fixture() as (clock, handled, facts, repos, prs, info):
                advanced = []
                original = S.step.handle

                def unsafe(repo, p, *args):
                    original(repo, p, *args)
                    if p['number'] == 1:
                        if cause == 'exception':
                            raise RuntimeError('PR evaluation failed')
                        S.T.stop(repo, 'withdrawal failed')
                    clock[0] += 9 * 60

                def safe_step(repo):
                    if repo not in S.T.STOPPED:
                        advanced.append(repo)

                with patch.object(S.T, 'seat', return_value='seated'), patch.object(S.T, 'step', safe_step):
                    with patch.object(S.step, 'handle', unsafe):
                        S.run_pass(['sam/r'])
                    C.WAKE.clear()
                    if mid_traversal:
                        def retry_one(*args):
                            original(*args)
                            clock[0] += 9 * 60

                        with patch.object(S.step, 'handle', retry_one):
                            S.run_pass(['sam/r'])
                        assert advanced == [], (cause, advanced)
                    else:
                        S.run_pass(['sam/r'], continuing=True)
                        assert advanced == [], (cause, advanced)
                        assert C.load('wake.json', {})['sam/r'] <= C.NOW + 5 * 60
                    C.WAKE.clear()
                    S.run_pass(['sam/r'], continuing=mid_traversal)
                assert advanced == ['sam/r'], (cause, mid_traversal, advanced)
                assert C.load('pass-progress.json', {})['repos'] == {}

    # Legacy saved stops are conservatively attached to all old receipts,
    # then retired by revalidation, rather than discarded during migration.
    with fixture() as (clock, handled, facts, repos, prs, info):
        C.save('pass-progress.json', {'repos': {'sam/r': {
            'done': {'1': 'old'}, 'stopped': 'old unsafe PR', 'seen_to': False}}})
        steps = []
        with patch.object(S.T, 'step', lambda repo: steps.append((repo, S.T.STOPPED.get(repo)))):
            S.run_pass(['sam/r'], continuing=True)
        assert steps == [('sam/r', None)], steps
        assert sorted(n for n, *_ in handled) == [1, 2], handled
    print('train stops remain safe and recover after cause revalidation: passed')

    # Both event lines and full catch-up honor a failed operation's durable
    # retry deadline, keeping its watchdog clock and cursor while ready
    # unrelated repositories can continue.
    for initial in (None, ['sam/r']):
        with fixture() as (clock, handled, facts, repos, prs, info):
            old = dict(repo='sam/r', what='sam/r', link='https://github.com/sam/r',
                       phase=W.FAILS, since=10, due=7210, kick='autoland/sam/r', claude=False, rev=None)
            C.save('watch.json', {'at': 10, 'items': {'sam/r': old}})
            attempts = []

            def unavailable(repo):
                attempts.append((repo, C.NOW))
                if repo == 'sam/r':
                    raise C.Unavailable('train unavailable')

            with patch.object(S.T, 'step', unavailable):
                S.run_pass(initial)
                assert C.load('wake.json', {})['sam/r'] == 1300
                cursor = copy.deepcopy(C.load('pass-progress.json', {})['repos']['sam/r'])
                repos.append({'repo': 'sam/ready', 'default': 'main'})
                for kick, continuing in ((['sam/r'], False), (['sam/r'], True),
                                         (None, True), (None, False)):
                    C.WAKE.clear()
                    C.NOW = 1021
                    S.run_pass(kick, continuing)
                    assert attempts.count(('sam/r', 1000)) == 1 and not any(r == 'sam/r' and t > 1000 for r, t in attempts), attempts
                    assert C.load('pass-progress.json', {})['repos']['sam/r'] == cursor
                    assert C.load('watch.json', {})['items']['sam/r'] == old
                    assert C.load('wake.json', {})['sam/r'] == 1300
                assert ('sam/ready', 1021) in attempts, attempts
                C.WAKE.clear()
                C.NOW = 1300
                S.run_pass(None, continuing=True)
                assert ('sam/r', 1300) in attempts, attempts
    print('full and kicked retries preserve deadlines, cursors and failure clocks: passed')

    for initial in (None, ['sam/r']):
        with fixture() as (clock, handled, facts, repos, prs, info):
            def limited(repo):
                C.LIMITED[:] = [1800]
                raise C.Unavailable('rate limited')

            with patch.object(S.T, 'step', limited):
                S.run_pass(initial)
            C.after_limit(initial)  # main's finally, on both handler lines
            assert all(at >= 1801 for at in C.load('continue.json', {}).values())
            attempts = []
            with patch.object(S.T, 'step', attempts.append):
                for kick, continuing in ((None, True), (None, False), (['sam/r'], False), (['sam/r'], True)):
                    C.WAKE.clear()
                    C.LIMITED.clear()
                    C.NOW = 1300
                    with patch.object(S.R, 'discover', side_effect=AssertionError('discovery before reset')):
                        S.run_pass(kick, continuing)
                    assert attempts == [], (initial, kick, continuing, attempts)
                    assert all(at >= 1801 for at in C.load('continue.json', {}).values())
                C.WAKE.clear()
                C.NOW = 1801
                S.run_pass(None, continuing=True)
            assert attempts == ['sam/r'], attempts
    print('rate limits gate both event lines and startup until reset: passed')
