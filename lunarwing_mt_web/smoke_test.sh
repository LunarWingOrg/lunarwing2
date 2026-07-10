#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"
export PYTHONPATH="$REPO_ROOT"

echo "-- Unit tests --"
python3 -c "
import unittest
from lunarwing_mt_web import tests
unittest.TextTestRunner(verbosity=2).run(
    unittest.TestLoader().loadTestsFromModule(tests)
)
"

echo ""
echo "-- Import check --"
python3 -c "
from lunarwing_mt_web.server import app
from lunarwing_mt_web.bridge import start_provisioning, SessionState
print('All modules imported successfully')
"

echo ""
echo "-- Flask test client --"
python3 -c "
from lunarwing_mt_web.server import app
c = app.test_client()
r = c.get('/')
assert r.status_code == 200, f'Expected 200, got {r.status_code}'
print('GET / ->', r.status_code, 'OK')
r = c.post('/api/validate/name', json={'name': 'test'})
assert r.status_code == 200
print('POST /api/validate/name ->', r.status_code, 'OK')
"

echo ""
echo "Smoke test passed"
