"""Hold an Apple High Performance session open while an RDP client takes control."""
import hashlib
import os
import secrets
import socket
import struct
import subprocess
import threading
import time
from collections import defaultdict
from pathlib import Path

import av
from Crypto.Cipher import AES
from isharescreen.proxy.media.nalu import reassemble_group
from isharescreen.proxy.protocol.offers import create_offers
from isharescreen.proxy.protocol.srtp import SRTCPEncryptor, SRTPDecryptor

HOST = "127.0.0.1"
CLIENT = "127.0.0.2"
VIDEO_PORT = 45992


def read(stream, count):
    data = stream.read(count)
    if len(data) != count:
        raise EOFError("RFB connection closed")
    return data


def field(stream, size):
    return read(stream, int.from_bytes(read(stream, size), "big"))


def login(sock, stream):
    banner = read(stream, 12)
    assert banner == b"RFB 003.889\n"
    sock.sendall(banner)
    assert field(stream, 1) == bytes([30])
    sock.sendall(bytes([30]))
    generator, size = struct.unpack(">HH", read(stream, 4))
    prime = int.from_bytes(read(stream, size), "big")
    public = int.from_bytes(read(stream, size), "big")
    private = secrets.randbits(256)
    shared = pow(public, private, prime).to_bytes(size, "big")
    key = hashlib.md5(shared).digest()
    credentials = b"".join(secrets.token_hex(12).encode().ljust(64, b"\0") for _ in range(2))
    sock.sendall(AES.new(key, AES.MODE_ECB).encrypt(credentials)
                 + pow(generator, private, prime).to_bytes(size, "big"))
    assert read(stream, 4) == bytes(4)
    sock.sendall(b"\x81")
    read(stream, 20)
    assert b"vvv" in field(stream, 4)


def negotiate(sock):
    sock.sendall(struct.pack(">BBHiii", 2, 0, 3, 1010, -223, 1105))
    display = bytearray(Path("/smoke/msg29.bin").read_bytes())
    struct.pack_into(">IIII", display, 12 + 0x9c, 1360, 900, 680, 450)
    sock.sendall(display)
    video, audio = create_offers()
    audio_keys = os.urandom(92)
    client_key, server_key = os.urandom(46), os.urandom(46)
    message = (struct.pack(">HI5H", 3, 3, len(audio), len(video), 0, 0, 0)
               + os.urandom(16) + audio_keys + audio + client_key + server_key + video)
    sock.sendall(b"\x1c\0" + struct.pack(">H", len(message)) + message)
    return SRTPDecryptor.from_blob(server_key), SRTCPEncryptor.from_blob(client_key)


def decode(udp, decryptor, feedback):
    decoder = av.CodecContext.create("hevc", "r")
    decoder.thread_count = 1
    groups = defaultdict(list)
    decoded = set()
    report = struct.pack(">BBHI", 128, 201, 1, 12345)
    next_report = 0
    deadline = time.monotonic() + 25
    while time.monotonic() < deadline:
        packet, source = udp.recvfrom(4096)
        if time.monotonic() >= next_report:
            udp.sendto(feedback.protect(report), source)
            next_report = time.monotonic() + 0.15
        if 200 <= packet[1] <= 207:
            continue
        header, payload = decryptor.decrypt(packet)
        ssrc = int.from_bytes(header[8:12], "big")
        groups[ssrc].append(payload)
        if header[1] & 128:
            nals = reassemble_group(groups.pop(ssrc))
            unit = b"".join(b"\0\0\0\1" + nal for nal in nals)
            for frame in decoder.decode(av.Packet(unit)):
                assert (frame.width, frame.height) == (1360, 240)
                decoded.add(ssrc & 3)
            if len(decoded) == 4:
                return
    raise TimeoutError("did not decode four HEVC strips")


with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as udp:
    udp.bind((CLIENT, VIDEO_PORT))
    udp.settimeout(10)
    udp.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4 << 20)
    with socket.create_connection((HOST, 45990), 10, (CLIENT, 0)) as sock:
        login(sock, sock.makefile("rb"))
        sock.settimeout(None)
        def drain_control():
            while sock.recv(65536):
                pass
        threading.Thread(target=drain_control, daemon=True).start()
        decode(udp, *negotiate(sock))
        print("Decoded all four Apple HEVC strips")
        subprocess.run(["rdp-probe"], check=True, timeout=45)
