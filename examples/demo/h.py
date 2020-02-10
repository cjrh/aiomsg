# microservice: H
import asyncio
import json
import time
from typing import Dict

from aiomsg import Søcket
from aiohttp import web
from dataclasses import dataclass


@dataclass
class Payload:
    msg_id: int
    req: Dict
    resp: Dict


REQ_COUNTER = 0
BACKEND_QUEUE = asyncio.Queue()  # TODO: must be inside async context
pending_backend_requests: {}


async def backend_receiver(sock: Søcket):
    async for msg in sock.messages():
        raw_data = json.loads(msg)
        data = Payload(**raw_data)
        f = pending_backend_requests.pop(data.msg_id)
        f.set_result(data.body)


async def backend(app):
    async with Søcket() as sock:
        await sock.bind("127.0.0.1", 25000)
        asyncio.create_task(backend_receiver(sock))
        while True:
            await sock.send(time.ctime().encode())
            await asyncio.sleep(1)


async def run_backend_job(sock: Søcket, data: Payload) -> Payload:
    f = asyncio.Future()
    REQ_COUNTER += 1
    pending_backend_requests[REQ_COUNTER] = f
    backend_response = await f
    data = dict(result=backend_response)


async def handle(request):
    nonlocal REQ_COUNTER
    # TODO: get post data from request to send to backend

    f = asyncio.Future()
    REQ_COUNTER += 1
    pending_backend_requests[REQ_COUNTER] = f
    backend_response = await f
    data = dict(result=backend_response)
    return web.json_response(data)


app = web.Application()
app.on_startup.append(backend)
app.add_routes([web.post("/process/{name}", handle)])

if __name__ == "__main__":
    web.run_app(app)
