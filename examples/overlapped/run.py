# Example: load balancer

import os
import sys
import shlex
import signal
from pathlib import Path
import subprocess as sp

this_dir = Path(__file__).parent


def make_arg(cmdline):
    return shlex.split(cmdline, posix=False)


# Start producer
s = f"{sys.executable} {this_dir / 'producer.py'}"
print(s)
producer = sp.Popen(make_arg(s))

# Start worker
s = f"{sys.executable} {this_dir / 'worker.py'}"
worker = sp.Popen(make_arg(s))

with producer, worker:
    try:
        producer.wait(100)
        worker.wait(100)
    except (KeyboardInterrupt, sp.TimeoutExpired):
        producer.send_signal(signal.CTRL_C_EVENT)
        worker.send_signal(signal.CTRL_C_EVENT)

        producer.wait(100)
        worker.wait(100)
