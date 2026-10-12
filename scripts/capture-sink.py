# /// script
# requires-python = ">=3.11"
# dependencies = ["dbus-fast>=5.2.0"]
# ///
"""DEV ONLY. Pose as an A2DP sink with the Px7 S2e's aptX Adaptive caps and record what a phone sends.

Registers SBC + aptX Adaptive sink endpoints and a pairing agent with BlueZ, logs the
configuration the phone picks and dumps every media packet to .scratch/phone-capture/
as `u16 LE length + raw bytes`. Normally started by scripts/capture-sink.sh.
"""

import asyncio
import os
import signal
import struct
import time
from pathlib import Path

from dbus_fast import BusType, Message, MessageType, Variant
from dbus_fast.aio import MessageBus
from dbus_fast.service import ServiceInterface, method

A2DP_SINK = "0000110b-0000-1000-8000-00805f9b34fb"
APTX_ADAPTIVE_CAPS = bytes.fromhex(
    "d7000000ad00190a376a00af644b0000aa" + "00" * 23
)  # spec/px7s2e-a2dp-caps.md sep9, 40 bytes
SBC_CAPS = bytes.fromhex("ffff0235")
ENDPOINTS = {
    "/capture/aptx_adaptive": (0xFF, APTX_ADAPTIVE_CAPS),
    "/capture/sbc": (0x00, SBC_CAPS),
}
AGENT_PATH = "/capture/agent"

OUT = Path(__file__).resolve().parent.parent / ".scratch" / "phone-capture"
STAMP = time.strftime("%Y%m%d-%H%M%S")


def log(msg: str) -> None:
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)


class Capture:
    def __init__(self, bus: MessageBus):
        self.bus = bus
        self.transports: dict[str, str] = {}  # transport path -> endpoint path
        self.fd: int | None = None
        self.fd_transport: str | None = None
        self.out = None
        self.count = 0
        self.bytes = 0

    async def acquire(self, transport: str, state: str) -> None:
        if self.fd is not None:
            return
        name = "TryAcquire" if state == "pending" else "Acquire"
        reply = await self.bus.call(
            Message(
                destination="org.bluez",
                path=transport,
                interface="org.bluez.MediaTransport1",
                member=name,
            )
        )
        if reply.message_type == MessageType.ERROR:
            log(f"{name} {transport} failed: {reply.error_name} {reply.body}")
            return
        idx, read_mtu, write_mtu = reply.body
        self.fd = reply.unix_fds[idx]
        self.fd_transport = transport
        os.set_blocking(self.fd, False)
        path = OUT / f"media-{STAMP}.bin"
        self.out = path.open("ab")
        log(f"acquired {transport} fd={self.fd} read_mtu={read_mtu} write_mtu={write_mtu} -> {path}")
        asyncio.get_running_loop().add_reader(self.fd, self.on_readable, read_mtu)

    def on_readable(self, mtu: int) -> None:
        try:
            data = os.read(self.fd, max(mtu, 4096))
        except BlockingIOError:
            return
        except OSError as e:
            log(f"read error: {e}")
            self.close_fd()
            return
        if not data:
            log("transport fd closed")
            self.close_fd()
            return
        self.out.write(struct.pack("<H", len(data)) + data)
        self.count += 1
        self.bytes += len(data)
        if self.count <= 5:
            log(f"packet {self.count}: {len(data)} bytes: {data[:24].hex(' ')}")
        if self.count % 100 == 0:
            self.out.flush()
            print(f"\r  {self.count} packets, {self.bytes} bytes", end="", flush=True)

    def close_fd(self) -> None:
        if self.fd is None:
            return
        asyncio.get_running_loop().remove_reader(self.fd)
        os.close(self.fd)
        self.fd = None
        if self.out:
            self.out.close()
            self.out = None
        print()
        log(f"closed capture: {self.count} packets, {self.bytes} bytes total")

    async def release(self) -> None:
        transport = self.fd_transport
        self.close_fd()
        if transport:
            await self.bus.call(
                Message(
                    destination="org.bluez",
                    path=transport,
                    interface="org.bluez.MediaTransport1",
                    member="Release",
                )
            )
            self.fd_transport = None

    def on_message(self, msg: Message) -> None:
        if (
            msg.message_type != MessageType.SIGNAL
            or msg.member != "PropertiesChanged"
            or msg.path not in self.transports
            or msg.body[0] != "org.bluez.MediaTransport1"
        ):
            return
        changed = msg.body[1]
        if "State" in changed:
            state = changed["State"].value
            log(f"transport {msg.path} state={state}")
            if state in ("pending", "active"):
                asyncio.ensure_future(self.acquire(msg.path, state))
            elif state == "idle" and self.fd_transport == msg.path:
                asyncio.ensure_future(self.release())


class Endpoint(ServiceInterface):
    def __init__(self, cap: Capture, path: str, codec: int, caps: bytes):
        super().__init__("org.bluez.MediaEndpoint1")
        self.cap, self.path, self.codec, self.caps = cap, path, codec, caps

    @method()
    def SetConfiguration(self, transport: "o", props: "a{sv}"):  # noqa: F821
        codec = props["Codec"].value if "Codec" in props else self.codec
        config = bytes(props["Configuration"].value) if "Configuration" in props else b""
        self.cap.transports[transport] = self.path
        lines = [
            f"endpoint: {self.path}",
            f"transport: {transport}",
            f"codec: 0x{codec:02x}",
            f"configuration ({len(config)} bytes): {config.hex(' ')}",
        ]
        for k, v in props.items():
            if k not in ("Codec", "Configuration"):
                lines.append(f"{k}: {v.value!r}")
        print("\n" + "=" * 72)
        log("SetConfiguration")
        for line in lines:
            print("  " + line)
        print("=" * 72, flush=True)
        path = OUT / f"config-{time.strftime('%Y%m%d-%H%M%S')}.txt"
        path.write_text("\n".join(lines) + "\n")
        log(f"saved {path}")
        state = props.get("State")
        if state and state.value in ("pending", "active"):
            asyncio.ensure_future(self.cap.acquire(transport, state.value))

    @method()
    def SelectConfiguration(self, caps: "ay") -> "ay":  # noqa: F821
        log(f"SelectConfiguration on {self.path}: {bytes(caps).hex(' ')}")
        return caps

    @method()
    def ClearConfiguration(self, transport: "o"):  # noqa: F821
        log(f"ClearConfiguration {transport}")
        self.cap.transports.pop(transport, None)
        if self.cap.fd_transport == transport:
            self.cap.close_fd()
            self.cap.fd_transport = None

    @method()
    def Release(self):
        log(f"Release {self.path}")


class Agent(ServiceInterface):
    """Auto-accepts everything: just-works pairing for the capture session."""

    def __init__(self):
        super().__init__("org.bluez.Agent1")

    @method()
    def Release(self):
        pass

    @method()
    def RequestPinCode(self, device: "o") -> "s":  # noqa: F821
        log(f"agent: PIN for {device} -> 0000")
        return "0000"

    @method()
    def DisplayPinCode(self, device: "o", pincode: "s"):  # noqa: F821
        log(f"agent: PIN {pincode} for {device}")

    @method()
    def RequestPasskey(self, device: "o") -> "u":  # noqa: F821
        return 0

    @method()
    def DisplayPasskey(self, device: "o", passkey: "u", entered: "q"):  # noqa: F821
        log(f"agent: passkey {passkey:06d} for {device}")

    @method()
    def RequestConfirmation(self, device: "o", passkey: "u"):  # noqa: F821
        log(f"agent: confirming passkey {passkey:06d} for {device} (accept it on the phone)")

    @method()
    def RequestAuthorization(self, device: "o"):  # noqa: F821
        log(f"agent: authorized {device}")

    @method()
    def AuthorizeService(self, device: "o", uuid: "s"):  # noqa: F821
        log(f"agent: authorized service {uuid} for {device}")

    @method()
    def Cancel(self):
        log("agent: cancelled")


async def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    bus = await MessageBus(bus_type=BusType.SYSTEM, negotiate_unix_fd=True).connect()
    cap = Capture(bus)
    bus.add_message_handler(cap.on_message)
    await bus.call(
        Message(
            destination="org.freedesktop.DBus",
            path="/org/freedesktop/DBus",
            interface="org.freedesktop.DBus",
            member="AddMatch",
            signature="s",
            body=["type='signal',sender='org.bluez',interface='org.freedesktop.DBus.Properties'"],
        )
    )

    intro = await bus.introspect("org.bluez", "/org/bluez")
    adapter = "/org/bluez/" + next(n.name for n in intro.nodes if n.name.startswith("hci"))
    media = bus.get_proxy_object(
        "org.bluez", adapter, await bus.introspect("org.bluez", adapter)
    ).get_interface("org.bluez.Media1")

    for path, (codec, caps) in ENDPOINTS.items():
        bus.export(path, Endpoint(cap, path, codec, caps))
        await media.call_register_endpoint(
            path,
            {
                "UUID": Variant("s", A2DP_SINK),
                "Codec": Variant("y", codec),
                "Capabilities": Variant("ay", caps),
            },
        )
        log(f"registered sink endpoint {path} codec=0x{codec:02x} caps={caps.hex(' ')}")

    bus.export(AGENT_PATH, Agent())
    agents = bus.get_proxy_object(
        "org.bluez", "/org/bluez", intro
    ).get_interface("org.bluez.AgentManager1")
    await agents.call_register_agent(AGENT_PATH, "DisplayYesNo")
    await agents.call_request_default_agent(AGENT_PATH)
    log("registered default pairing agent (auto-accept)")
    log(f"waiting for the phone; captures go to {OUT}  (Ctrl-C to stop)")

    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, stop.set)
    await stop.wait()

    print()
    log("stopping")
    await cap.release()
    for path in ENDPOINTS:
        try:
            await media.call_unregister_endpoint(path)
        except Exception as e:  # noqa: BLE001
            log(f"unregister {path}: {e}")
    try:
        await agents.call_unregister_agent(AGENT_PATH)
    except Exception as e:  # noqa: BLE001
        log(f"unregister agent: {e}")
    bus.disconnect()


if __name__ == "__main__":
    asyncio.run(main())
