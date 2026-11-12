"""Slow PRs must not make every full pass restart at the same first PR."""
import copy
from contextlib import ExitStack
import json
import os
from pathlib import Path
import sys
import tempfile
from unittest.mock import patch

sys.path.insert(0, sys.argv[1] if len(sys.argv) > 1 else
                str(Path(__file__).resolve().parents[2] / 'modules/autoland'))
with tempfile.TemporaryDirectory() as tmp:
    cfg = Path(tmp) / 'config.json'
    cfg.write_text(json.dumps(dict(
        context='test', stateDir=tmp, snapDir=tmp,
        conary={'cn': 'cn', 'env': {}}, trainContext='train', baselineJobs=False,
        labels={'first': 'first', 'waiting': {'ci': {'name': 'waiting: CI'}}},
        watch={'kick': 'watchdog', 'escalated': f'{tmp}/escalated.json'},
    )))
    os.environ['AUTOLAND_CONFIG'] = str(cfg)
    import common as C
    import steward as S
    import waits as W

    repos = [{'repo': f'sam/{n}', 'default': 'main'} for n in 'abc']
    info = {'defaultBranchRef': {'name': 'main', 'target': {'oid': 'base'}}}

    def pr(n):
        return dict(number=n, title=str(n), headRefOid=f'head-{n}', headRefName=f'branch-{n}',
                    baseRefName='main', labels={'nodes': []}, url=f'https://github.com/sam/b/pull/{n}')

    prs = {'a': [pr(1)], 'b': [pr(n) for n in range(1, 8)], 'c': [pr(1)]}
    clock, handled, trains, branches, hooks, publications = [0], [], [], [], [], []

    def handle(repo, pr, info, mine, *args):
        handled.append((repo, pr['number']))
        clock[0] += 200  # nine normal steps take thirty minutes in total
        if repo == 'sam/b' and pr['number'] == 1:
            C.wake(repo, C.NOW + 10000)
            S.T.stop(repo, 'a settlement could not be withdrawn')
        if repo == 'sam/a':
            mine.append(('Owner', f'[{repo}#1](url)', 'waiting on owner'))
        else:
            W.NOTED[repo][W.key_of(repo, pr)] = W.item(repo, pr, 'CI', 120, f'autoland/{repo}', pr['headRefOid'])

    def coverage(repos, covered):
        done = {r['repo'] for r in covered}
        return {'at': C.NOW, 'repos': {r['repo']: {'covered': r['repo'] in done} for r in repos}}

    def train(repo):
        if repo == 'sam/b':
            assert S.T.STOPPED.get(repo) == 'a settlement could not be withdrawn'
        trains.append(repo)

    def publish(rows):
        publications.append(rows)
        if len(publications) == 1:
            raise C.Unavailable('GitHub failed after the local checkpoint')

    with ExitStack() as stack:
        for obj, name, value in (
            (S.R, 'discover', lambda: repos), (S.R, 'restart', lambda *a: None),
            (S.R, 'settle_repo', lambda *a: False), (S.R, 'watch_default', lambda *a: {}),
            (S.R, 'publish', publish), (S.R, 'hooks', hooks.append),
            (S, 'open_prs', lambda owner, name: (info, copy.deepcopy(prs[name]))),
            (S.P, 'begin', lambda *a: None), (S.P, 'place', lambda repo, n: n),
            (S.T, 'begin', lambda repo: S.T.STOPPED.pop(repo, None)), (S.T, 'on', lambda *a: True),
            (S.T, 'step', train), (S.step, 'handle', handle),
            (S, 'branches', lambda repo, *a, **k: branches.append(repo)),
            (S, 'upkeep', lambda *a: None), (S.V, 'sweep', lambda *a: None),
            (C, 'follow_renames', lambda names: None), (C, 'taken', lambda *a: None),
            (C, 'coverage', coverage), (C, 'poke', lambda *a: None),
        ):
            stack.enter_context(patch.object(obj, name, value))
        # The production monotonic clock is virtual: this checks the real
        # run_pass loop without waiting half an hour or requiring GitHub.
        stack.enter_context(patch('time.monotonic', side_effect=lambda: clock[0]))
        C.NOW = 1000
        C.save('watch.json', {'at': 1000, 'items': {'sam/b#7': dict(repo='sam/b', since=10, rev='head-7', phase='CI')}})
        C.save('digest-repos.json', {'sam/c': [['Owner', '[sam/c#1](url)', 'untouched']]})
        for turn in range(4):
            C.WAKE.clear()
            try:
                if turn:
                    S.run_pass(None, continuing=True)
                else:
                    S.run_pass(None)
            except C.Unavailable:
                assert turn == 0
            if turn == 0:
                assert len(handled) == 3, ('the pass ignored its work budget', handled)
                assert trains == branches == ['sam/a'], (trains, branches)
                assert C.load('watch.json', {})['items']['sam/b#7']['since'] == 10
                assert C.load('digest-repos.json', {})['sam/c'][0][2] == 'untouched'
            if turn < 3:
                assert C.load('continue.json', {})[''] <= C.NOW + 1
                assert hooks == [], hooks
            C.NOW += 601
        expected = [('sam/a', 1), *[('sam/b', n) for n in range(1, 8)], ('sam/c', 1)]
        assert handled == expected, (handled, expected)
        assert trains == branches == ['sam/a', 'sam/b', 'sam/c'], (trains, branches)
        assert hooks == [repos], hooks
        assert all(x['covered'] for x in C.load('repos.json', {})['repos'].values())
        assert C.load('digest-repos.json', {})['sam/a'][0][0] == 'Owner'
        assert C.load('watch.json', {})['items']['sam/b#7']['since'] == 10
        assert len(publications) == 4
        assert C.load('wake.json', {})['sam/b'] == 11000

    # A cursor cannot carry a settlement past changed decision inputs.
    from batch import Batch
    batch = Batch(['sam/b'], continuing=True)
    original = pr(1)
    batch.note('sam/b', original, [], True, info)
    assert batch.pending('sam/b', [original], [], info) == []
    assert batch.visited('sam/b', original) and not batch.visited('sam/b', pr(2))
    for field, value in [('headRefOid', 'new'), ('baseRefName', 'release'),
                         ('labels', {'nodes': [{'name': 'hold'}]}),
                         ('reviewThreads', {'nodes': [{'isResolved': False}]}),
                         ('body', 'changed description')]:
        moved = {**original, field: value}
        assert batch.pending('sam/b', [moved], [], info) == [moved], field
    ours = {**original, 'updatedAt': 'now', 'labels': {'nodes': [{'name': 'waiting: CI'}]}}
    assert batch.pending('sam/b', [ours], [], info) == []
    assert batch.pending('sam/b', [original], [], {'defaultBranchRef': {'target': {'oid': 'new'}}}) == [original]
    with patch.dict(C.CFG, {'changedPolicy': True}):
        assert batch.pending('sam/b', [original], [], info) == [original]
    # Missing fields are left for the existing per-PR error boundary.
    malformed = {k: v for k, v in original.items() if k != 'labels'}
    assert batch.pending('sam/b', [malformed], [], info) == [malformed]
    # Only our own status is ignored. A new external red check is an input.
    with_status = {**original, 'commits': {'nodes': [{'commit': {'statusCheckRollup': {
        'contexts': {'nodes': [{'context': C.CONTEXT, 'description': 'old'}]}}}}]}}
    batch.note('sam/b', with_status, [], True, info)
    updated = copy.deepcopy(with_status)
    updated['commits']['nodes'][0]['commit']['statusCheckRollup']['contexts']['nodes'][0]['description'] = 'new'
    assert batch.pending('sam/b', [updated], [], info) == []
    updated['commits']['nodes'][0]['commit']['statusCheckRollup']['contexts']['nodes'].append({'context': 'ci', 'state': 'FAILURE'})
    assert batch.pending('sam/b', [updated], [], info) == [updated]
print('bounded steward batches: passed')
