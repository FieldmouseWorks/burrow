"""Remote calls cannot silently consume the whole steward unit deadline."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from unittest.mock import patch

sys.path.insert(0, sys.argv[1] if len(sys.argv) > 1 else
                str(Path(__file__).resolve().parents[2] / 'modules/autoland'))
with tempfile.TemporaryDirectory() as tmp:
    config = Path(tmp) / 'config.json'
    config.write_text(json.dumps(dict(context='test', stateDir=tmp,
                                     conary={'cn': 'cn', 'env': {}}, trainContext='train')))
    os.environ['AUTOLAND_CONFIG'] = str(config)
    import common as C
    import train as T

    def stalled(command, **kwargs):
        # Fails on the old unbounded calls, without hanging the test.
        assert 0 < kwargs.get('timeout', 0) <= 300, command
        raise subprocess.TimeoutExpired(command, kwargs['timeout'], output='private output')

    calls = [lambda: C.gh('api', 'private-body', check=False),
             lambda: C.fresh('private-path'),
             lambda: C.limited('rate limit', {}),
             lambda: T.cn('train', 'step', 'sam/r')]
    # Admission is covered by paused.py; these calls must reach the remote
    # operation so each one's production deadline is exercised.
    with patch.object(C, 'paused', return_value=False), patch.object(C, 'gh_env', return_value={}), patch.object(C.subprocess, 'run', side_effect=stalled):
        for call in calls:
            C.LIMITED.clear()
            try:
                call()
            except C.Unavailable as error:
                assert 'timed out after' in str(error), error
                assert 'private' not in str(error), error
            else:
                raise AssertionError('timeout must defer through the unavailable path')

    # Exercise subprocess.run's real timeout and cleanup, too.
    try:
        C.remote([sys.executable, '-c', 'import time; time.sleep(30)'], timeout=0.02,
                 capture_output=True, text=True)
    except C.Unavailable:
        pass
    else:
        raise AssertionError('sleeping child was not bounded')

    result = C.remote([sys.executable, '-c', 'print(42)'], capture_output=True, text=True)
    assert result.returncode == 0 and result.stdout == '42\n', result
print('remote call deadlines: passed')
