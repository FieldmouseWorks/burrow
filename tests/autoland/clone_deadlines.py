"""Real clone descendants, publication, retries and preservation of local data.

Run with the steward module directory and the burrow-upkeep executable.
"""
import json
from concurrent.futures import ThreadPoolExecutor
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
from unittest.mock import patch

sys.path.insert(0, sys.argv[1])
upkeep = str(Path(sys.argv[2]).resolve())
with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    src = root / 'projects'
    src.mkdir()
    config = root / 'config.json'
    config.write_text(json.dumps(dict(
        context='test', stateDir=tmp, projectsDir=str(src), upkeep=[upkeep, str(config)],
        proj=str(root / 'proj'), jobTimeout=10, jobIdle=10,
    )))
    os.environ['AUTOLAND_CONFIG'] = str(config)
    import common as C

    # gh -> git -> transport. Each records its PID, including a transport
    # ignoring TERM. The phases cover no destination yet, a .git with origin
    # during fetch, and a checkout that has begun populating the worktree.
    fake = root / 'gh'
    fake.write_text(f'''#!{sys.executable}
import os, pathlib, signal, subprocess, sys, time
root = pathlib.Path({tmp!r})
with (root / 'pids').open('a') as f:
    f.write(str(os.getpid()) + '\\n')
phase = os.environ['CLONE_PHASE']
if sys.argv[1] == 'repo':
    repo, path = sys.argv[3:5]
    if phase == 'success':
        subprocess.run(['git', 'init', '-q', path], check=True)
        subprocess.run(['git', '-C', path, 'remote', 'add', 'origin', 'https://github.com/' + repo], check=True)
        pathlib.Path(path, 'completed').write_text('ready')
        sys.exit(0)
    child = subprocess.Popen([sys.executable, __file__, 'git', repo, path])
    if phase == 'failed-parent':
        while not (root / 'ready').exists():
            time.sleep(.005)
        sys.exit(1)
elif sys.argv[1] == 'git':
    repo, path = sys.argv[2:4]
    child = subprocess.Popen([sys.executable, __file__, 'transport', repo, path])
else:
    repo, path = sys.argv[2:4]
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    if phase != 'before-directory':
        subprocess.run(['git', 'init', '-q', path], check=True)
        subprocess.run(['git', '-C', path, 'remote', 'add', 'origin', 'https://github.com/' + repo], check=True)
    if phase in ('checkout', 'failed-parent'):
        pathlib.Path(path, 'partial').write_text('unfinished')
    if phase == 'destination-race':
        final = pathlib.Path(os.environ['CLONE_DEST'])
        final.mkdir(exist_ok=True)
        (final / 'keep').write_text('local data')
        pathlib.Path(path, 'completed').write_text('ready')
        sys.exit(0)
    (root / 'ready').touch()
    if phase == 'slow-success':
        while not (root / 'release').exists():
            time.sleep(.005)
        pathlib.Path(path, 'completed').write_text('ready')
        sys.exit(0)
    time.sleep(60)
    sys.exit(0)
child.wait()
sys.exit(child.returncode)
''')
    fake.chmod(0o755)
    proj = root / 'proj'
    proj.write_text(f'''#!{sys.executable}
import pathlib, sys
path = pathlib.Path({str(src)!r}, sys.argv[-2])
assert (path / 'completed').read_text() == 'ready', path
with open({str(root / 'jobs')!r}, 'a') as f:
    f.write(sys.argv[-2] + '\\n')
print('job-1')
''')
    proj.chmod(0o755)

    def pids():
        return [int(x) for x in (root / 'pids').read_text().split()]

    def reset():
        for name in ('pids', 'ready', 'jobs', 'release'):
            (root / name).unlink(missing_ok=True)

    def no_children():
        assert len(pids()) >= 3, pids()
        # Absence, rather than merely a zombie state, proves reaping too.
        assert not [pid for pid in pids() if Path(f'/proc/{pid}').exists()], pids()

    with patch.dict(os.environ, PATH=f'{root}:{os.environ["PATH"]}'), \
            patch.object(C, 'paused', return_value=False), \
            patch.object(C, 'follow_renames', lambda *a: None), \
            patch.object(C, 'retiring', return_value=False):
        try:
            for role in ('baseline', 'flaky'):
                for phase in ('before-directory', 'fetch', 'checkout', 'failed-parent'):
                    for empty in (False, True):
                        reset()
                        repo = f'sam/{role}-{phase}-{empty}'
                        final = Path(C.destination(repo))
                        if empty:
                            final.mkdir()
                        with patch.dict(os.environ, CLONE_PHASE=phase), C.pass_deadline(.5):
                            assert C.job(repo, 'task', clone=True, role=role) is None, (role, phase)
                        assert not (root / 'jobs').exists(), (role, phase)
                        assert C.project_dir(repo) is None, (role, phase)
                        assert C.job(repo, 'task', role=role) is None
                        no_children()
                        assert final.is_dir() == empty and not list(src.glob('.autoland-clone-*')), list(src.iterdir())
                        if empty:
                            assert not list(final.iterdir()), final
                        assert C.taken(repo) is None
                        # The next pass retries and only then hands a completed
                        # tree to proj, with no partial checkout to adopt.
                        with patch.dict(os.environ, CLONE_PHASE='success'), C.pass_deadline(2):
                            assert C.job(repo, 'task', clone=True, role=role) == 'job-1'
                        assert (root / 'jobs').read_text().split() == [final.name]
                        assert (final / 'completed').exists()
                        print(f'clone deadline: {role} {phase}, empty={empty}: passed')

            for role in ('baseline', 'flaky'):
                reset()
                repo = f'sam/slow-success-{role}'
                final = Path(C.destination(repo))
                final.mkdir()

                def observe():
                    until = time.monotonic() + 3
                    while not (root / 'ready').exists() and time.monotonic() < until:
                        time.sleep(.005)
                    try:
                        assert (root / 'ready').exists(), 'clone did not reach checkout'
                        return C.project_dir(repo), list(final.iterdir())
                    finally:
                        (root / 'release').touch()

                with ThreadPoolExecutor(max_workers=1) as pool, \
                        patch.dict(os.environ, CLONE_PHASE='slow-success'), C.pass_deadline(4):
                    observed = pool.submit(observe)
                    assert C.job(repo, 'task', clone=True, role=role) == 'job-1'
                    assert observed.result() == (None, []), observed.result()
                no_children()
                assert (final / 'completed').exists()

            # Existing files, symlinks and nonempty directories stay intact.
            for kind in ('file', 'directory', 'symlink'):
                reset()
                repo = f'sam/existing-{kind}'
                final = Path(C.destination(repo))
                if kind == 'file':
                    final.write_text('local data')
                elif kind == 'directory':
                    final.mkdir()
                    (final / 'keep').write_text('local data')
                else:
                    final.symlink_to(src / 'existing-directory', target_is_directory=True)
                with patch.dict(os.environ, CLONE_PHASE='success'), C.pass_deadline(2):
                    assert C.job(repo, 'task', clone=True, role='baseline') is None
                assert not (root / 'pids').exists(), kind
                assert (final if kind == 'file' else final / 'keep').read_text() == 'local data'

            # A destination filled while the clone is running is preserved,
            # too. Successful gh still must not publish over that data.
            reset()
            repo = 'sam/destination-race'
            final = Path(C.destination(repo))
            final.mkdir()
            with patch.dict(os.environ, CLONE_PHASE='destination-race', CLONE_DEST=str(final)), C.pass_deadline(2):
                assert C.job(repo, 'task', clone=True, role='flaky') is None
            no_children()
            assert (final / 'keep').read_text() == 'local data'
            assert not (final / 'completed').exists()

            # An exhausted pass starts no clone and leaves an empty folder.
            reset()
            repo = 'sam/expired'
            final = Path(C.destination(repo))
            final.mkdir()
            with C.pass_deadline(0):
                assert C.checkout(repo) is None
            assert not (root / 'pids').exists() and not list(final.iterdir())
        finally:
            # Clean up real descendants if this test is run on buggy code.
            if (root / 'pids').exists():
                for pid in pids():
                    try:
                        os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
print('clone publication, descendants, retry and preservation: passed')
