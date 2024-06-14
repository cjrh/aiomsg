import asyncio
import logging
import signal

import aiorun
import aiomsg
from concurrent.futures import ProcessPoolExecutor as Executor


def fib(n):
    if n < 2:
        return n
    return fib(n - 2) + fib(n - 1)


async def fetch_work(sock: aiomsg.Søcket) -> dict:
    await sock.send(b"1")
    work = await sock.recv_json()
    return work


async def main(executor):
    async with aiomsg.Søcket() as sock:
        await sock.connect()
        while True:
            # "Fetching" is actually a two-step process, a send followed
            # by a receive, and we don't want to allow shutdown between
            # those two operations. That's why there's a guard.
            work = await fetch_work(sock)
            logging.info(f"Worker received work: {work}")
            # CPU-bound task MUST be run in an executor, otherwise
            # heartbeats inside aiomsg will fail.
            executor_job = asyncio.get_running_loop().run_in_executor(
                executor, fib, work["n"]
            )
            result = await executor_job
            logging.info(f"Job completed, the answer is: {result}")


def initializer():
    """Disable the handler for KeyboardInterrupt in the pool of
    executors.

    NOTE that the initializer must not be inside the __name__ guard,
    since the child processes need to be able to execute it. """
    signal.signal(signal.SIGINT, signal.SIG_IGN)


# This guard is SUPER NECESSARY if using ProcessPoolExecutor on Windows
if __name__ == "__main__":
    logging.basicConfig(level="INFO")
    executor = Executor(max_workers=4, initializer=initializer)
    aiorun.run(main(executor))
