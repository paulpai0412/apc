"""Transport-only ingress to the isolated HMI; no configurable destination."""
import asyncio

slots = asyncio.Semaphore(64)

async def copy(source, destination):
    while chunk := await source.read(65536):
        destination.write(chunk)
        await destination.drain()

async def handle(reader, writer):
    upstream = None
    tasks = []
    try:
        async with slots:
            remote, upstream = await asyncio.wait_for(asyncio.open_connection('hmi', 3000), timeout=5)
            tasks = [asyncio.create_task(copy(reader, upstream)), asyncio.create_task(copy(remote, writer))]
            await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
    except (OSError, asyncio.TimeoutError):
        pass
    finally:
        for task in tasks:
            task.cancel()
        if tasks:
            await asyncio.gather(*tasks, return_exceptions=True)
        for connection in (upstream, writer):
            if connection is not None:
                connection.close()
                try:
                    await connection.wait_closed()
                except OSError:
                    pass

async def main():
    server = await asyncio.start_server(handle, '0.0.0.0', 3000, backlog=64)
    async with server:
        await server.serve_forever()

asyncio.run(main())
