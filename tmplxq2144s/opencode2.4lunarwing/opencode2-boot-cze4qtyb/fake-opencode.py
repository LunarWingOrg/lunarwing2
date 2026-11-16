#!/usr/bin/env python3
import json
import os
import signal
import sys
import time

job = {"id": "sh_train", "kind": "shell", "title": "python train.py"}
def emit(event_type, timestamp):
    print(json.dumps({
        "type": event_type,
        "timestamp": timestamp,
        "sessionID": "ses_1",
        "jobs": [job],
        "credential_present": "AGENT_AUTH_TOKEN" in os.environ,
    }), flush=True)

# GNU timeout signals both the child and its process group, so a loaded host
# can enter this handler twice. Ignore repeats before writing; the self-signal
# makes that scheduling race deterministic for this regression test.
def cancelled(signum, frame):
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    os.kill(os.getpid(), signal.SIGTERM)
    emit("test_cancelled", 2)
    sys.exit(143)

signal.signal(signal.SIGTERM, cancelled)
emit("background_wait", 1)
with open(os.environ["FAKE_CLI_READY_FILE"], "w") as marker:
    marker.write("ready\n")
while True:
    time.sleep(60)
