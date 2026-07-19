#!/usr/bin/env python3
"""Minimal loopback IRC server for live WeeChat channel validation."""

import argparse
import asyncio
from collections import defaultdict


class Client:
    def __init__(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter):
        self.reader = reader
        self.writer = writer
        self.nick = ""
        self.user = "user"
        self.registered = False
        self.channels: set[str] = set()

    async def send(self, line: str) -> None:
        self.writer.write(f"{line}\r\n".encode())
        await self.writer.drain()

    @property
    def prefix(self) -> str:
        return f"{self.nick}!{self.user}@localhost"


class IrcFixture:
    def __init__(self) -> None:
        self.clients: dict[str, Client] = {}
        self.channels: dict[str, set[Client]] = defaultdict(set)

    async def register(self, client: Client) -> None:
        if client.registered or not client.nick or not client.user:
            return
        client.registered = True
        self.clients[client.nick.casefold()] = client
        await client.send(f":fixture 001 {client.nick} :Welcome to the CHPAR IRC fixture")
        await client.send(f":fixture 002 {client.nick} :Your host is fixture")
        await client.send(
            f":fixture 005 {client.nick} CASEMAPPING=rfc1459 CHANTYPES=#& PREFIX=(ov)@+ :are supported"
        )
        await client.send(f":fixture 376 {client.nick} :End of MOTD")
        print(f"REGISTER {client.nick}", flush=True)

    async def join(self, client: Client, channel: str) -> None:
        channel = channel.split(",", 1)[0]
        client.channels.add(channel)
        self.channels[channel.casefold()].add(client)
        line = f":{client.prefix} JOIN :{channel}"
        await asyncio.gather(*(member.send(line) for member in self.channels[channel.casefold()]))
        names = " ".join(sorted(member.nick for member in self.channels[channel.casefold()]))
        await client.send(f":fixture 353 {client.nick} = {channel} :{names}")
        await client.send(f":fixture 366 {client.nick} {channel} :End of NAMES")
        print(f"JOIN {client.nick} {channel}", flush=True)

    async def privmsg(self, client: Client, target: str, text: str) -> None:
        line = f":{client.prefix} PRIVMSG {target} :{text}"
        if target.startswith(("#", "&")):
            recipients = [
                member
                for member in self.channels[target.casefold()]
                if member is not client
            ]
        else:
            recipient = self.clients.get(target.casefold())
            recipients = [recipient] if recipient is not None else []
        if recipients:
            await asyncio.gather(*(recipient.send(line) for recipient in recipients))
        print(f"PRIVMSG {client.nick} {target} :{text}", flush=True)

    async def handle(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        client = Client(reader, writer)
        try:
            while raw := await reader.readline():
                line = raw.decode(errors="replace").rstrip("\r\n")
                if not line:
                    continue
                command, _, rest = line.partition(" ")
                command = command.upper()
                if command == "CAP":
                    subcommand = rest.split(" ", 1)[0].upper()
                    if subcommand == "LS":
                        await client.send(":fixture CAP * LS :")
                    elif subcommand == "REQ":
                        await client.send(f":fixture CAP * NAK :{rest.partition(':')[2]}")
                elif command == "NICK":
                    client.nick = rest.lstrip(":").split(" ", 1)[0]
                    await self.register(client)
                elif command == "USER":
                    client.user = rest.split(" ", 1)[0]
                    await self.register(client)
                elif command == "PING":
                    await client.send(f"PONG {rest}")
                elif command == "JOIN":
                    await self.join(client, rest.lstrip(":"))
                elif command == "PRIVMSG":
                    target, _, text = rest.partition(" :")
                    await self.privmsg(client, target, text)
                elif command == "WHO":
                    target = rest.split(" ", 1)[0]
                    for member in self.channels[target.casefold()]:
                        await client.send(
                            f":fixture 352 {client.nick} {target} {member.user} localhost fixture {member.nick} H :0 {member.nick}"
                        )
                    await client.send(f":fixture 315 {client.nick} {target} :End of WHO")
                elif command == "QUIT":
                    break
        finally:
            if client.nick:
                self.clients.pop(client.nick.casefold(), None)
            for channel in client.channels:
                self.channels[channel.casefold()].discard(client)
            writer.close()
            await writer.wait_closed()


async def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, required=True)
    args = parser.parse_args()
    fixture = IrcFixture()
    server = await asyncio.start_server(fixture.handle, args.host, args.port)
    print(f"LISTEN {args.host}:{args.port}", flush=True)
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    asyncio.run(main())
