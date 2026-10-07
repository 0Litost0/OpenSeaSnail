#!/usr/bin/env python3
"""Exercise the native sidecar through its inherited-FD HTTP contract."""

import argparse
import hashlib
import http.client
import io
import json
import os
import select
import socket
import struct
import subprocess
import math
import time
import wave


def request(port, capability, method, path, body=None, content_type=None, timeout=300):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
    headers = {
        "X-SeaSnail-Capability": capability,
        "X-SeaSnail-Protocol-Version": "1",
    }
    if content_type:
        headers["Content-Type"] = content_type
    conn.request(method, path, body=body, headers=headers)
    response = conn.getresponse()
    payload = response.read(16 * 1024 * 1024 + 1)
    conn.close()
    if len(payload) > 16 * 1024 * 1024:
        raise RuntimeError("response exceeded protocol limit")
    return response.status, json.loads(payload)


def assert_auth_precedes_body(port):
    """A rejected peer must not make the sidecar read/allocate its declared body."""
    with socket.create_connection(("127.0.0.1", port), timeout=3) as sock:
        sock.sendall(
            b"POST /v1/transcribe HTTP/1.1\r\n"
            b"Host: 127.0.0.1\r\n"
            b"X-SeaSnail-Capability: invalid\r\n"
            b"X-SeaSnail-Protocol-Version: 1\r\n"
            b"Content-Type: audio/wav\r\n"
            b"Content-Length: 134217728\r\n\r\n"
        )
        response = sock.recv(4096)
    if not response.startswith(b"HTTP/1.1 401 "):
        raise RuntimeError("capability was not checked before request body")


def assert_declared_size_limit(port, capability):
    """A declared Content-Length beyond the 128 MiB cap is rejected at header time."""
    with socket.create_connection(("127.0.0.1", port), timeout=3) as sock:
        sock.sendall(
            b"POST /v1/transcribe HTTP/1.1\r\n"
            b"Host: 127.0.0.1\r\n"
            + f"X-SeaSnail-Capability: {capability}\r\n".encode("ascii")
            + b"X-SeaSnail-Protocol-Version: 1\r\n"
            b"Content-Type: audio/wav\r\n"
            b"Content-Length: 134217729\r\n\r\n"
        )
        response = sock.recv(4096)
    if not response.startswith(b"HTTP/1.1 413 "):
        raise RuntimeError("declared oversize body was not rejected with 413")


# Deterministic empty-result fixtures. Both are 1 s of 16 kHz mono PCM16: pure
# digital silence, and LCG noise with a peak amplitude of 6 LSB (~-74 dBFS),
# far below the Silero VAD trigger level. Parameters and SHA-256 are pinned so
# any regeneration drift fails here instead of at the transcribe assertions.
FIXTURE_SAMPLE_RATE = 16000
FIXTURE_NOISE_AMPLITUDE = 6
FIXTURE_NOISE_SEED = 20260924
SILENCE_SHA256 = "643f8a8dc8bd9c19225afffad2becfec5426180b3749cb208abdf1a6c8354efc"
NOISE_SHA256 = "9548d518d8117bf53ce150e38cf8358e96568472d6ee8a7c9663d8fc694974f2"


def pcm16_wav(samples):
    buffer = io.BytesIO()
    with wave.open(buffer, "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(FIXTURE_SAMPLE_RATE)
        wav.writeframes(struct.pack(f"<{len(samples)}h", *samples))
    return buffer.getvalue()


def silence_fixture():
    return pcm16_wav([0] * FIXTURE_SAMPLE_RATE)


def noise_fixture():
    state = FIXTURE_NOISE_SEED
    samples = []
    for _ in range(FIXTURE_SAMPLE_RATE):
        state = (state * 6364136223846793005 + 1442695040888963407) & 0xFFFFFFFFFFFFFFFF
        samples.append(((state >> 33) % (2 * FIXTURE_NOISE_AMPLITUDE + 1)) - FIXTURE_NOISE_AMPLITUDE)
    return pcm16_wav(samples)


def pinned_fixture(samples_wav, expected_sha256, label):
    digest = hashlib.sha256(samples_wav).hexdigest()
    if digest != expected_sha256:
        raise RuntimeError(f"{label} fixture drifted: sha256={digest}")
    return samples_wav


CATALOG_ID = "sensevoice-small-sherpa-int8"


def assert_empty_success(port, capability, wav_bytes, label):
    """No-speech input must return the legal empty variant, not a 500."""
    status, result = request(port, capability, "POST", "/v1/transcribe", wav_bytes, "audio/wav")
    if status != 200:
        raise RuntimeError(f"{label}: expected 200 empty success, got status {status}")
    if result.get("protocol_version") != 1 or result.get("catalog_id") != CATALOG_ID:
        raise RuntimeError(f"{label}: empty response lost protocol identity")
    if result.get("text") != "" or result.get("segments") != []:
        raise RuntimeError(f"{label}: expected empty text and empty segments")
    transforms = result.get("transforms") or {}
    if (transforms.get("use_itn") is not True
            or transforms.get("rule_fsts") is not False
            or transforms.get("homophone_replacer") is not False
            or transforms.get("post_decode_text_modified") is not False):
        raise RuntimeError(f"{label}: empty response lost transforms metadata")


def assert_header_deadline_is_not_sliding(port):
    """One byte at a time must not keep an unauthenticated connection alive."""
    with socket.create_connection(("127.0.0.1", port), timeout=3) as sock:
        started = time.monotonic()
        next_byte = started
        while time.monotonic() - started < 7:
            now = time.monotonic()
            if now >= next_byte:
                try:
                    sock.sendall(b"G")
                except (BrokenPipeError, ConnectionResetError):
                    return
                next_byte += 1
            readable, _, _ = select.select([sock], [], [], min(0.2, max(0, next_byte - now)))
            if readable:
                response = sock.recv(4096)
                if response and not response.startswith(b"HTTP/1.1 400 "):
                    raise RuntimeError("unexpected slow-header response")
                if time.monotonic() - started > 6.5:
                    raise RuntimeError("header total deadline slid with received bytes")
                return
    raise RuntimeError("slow header was not closed by total deadline")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--sidecar", required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument("--tokens", required=True)
    parser.add_argument("--vad", required=True)
    parser.add_argument("--wav", required=True)
    args = parser.parse_args()

    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.bind(("127.0.0.1", 0))
    listener.listen(16)
    capability = os.urandom(32).hex()
    read_fd, write_fd = os.pipe()
    process = subprocess.Popen(
        [
            args.sidecar,
            "--listener-fd", str(listener.fileno()),
            "--capability-fd", str(read_fd),
            "--catalog-id", CATALOG_ID,
            "--model", args.model,
            "--tokens", args.tokens,
            "--vad-model", args.vad,
            "--num-threads", "1",
        ],
        pass_fds=(listener.fileno(), read_fd),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    os.close(read_fd)
    os.write(write_fd, capability.encode("ascii"))
    os.close(write_fd)
    port = listener.getsockname()[1]
    try:
        status, health = request(port, capability, "GET", "/health", timeout=30)
        if status != 200 or health.get("status") != "ready":
            raise RuntimeError("health contract failed")
        assert_auth_precedes_body(port)
        assert_declared_size_limit(port, capability)
        assert_header_deadline_is_not_sliding(port)
        with open(args.wav, "rb") as wav_file:
            wav = wav_file.read()
        status, result = request(port, capability, "POST", "/v1/transcribe", wav, "audio/wav")
        segments = result.get("segments", [])
        tokens = sum(len(segment.get("decoded_tokens", [])) for segment in segments)
        if status != 200 or not result.get("text") or not segments or tokens == 0:
            raise RuntimeError("transcription contract failed")
        if any(segment["end_seconds"] <= segment["start_seconds"] for segment in segments):
            raise RuntimeError("invalid segment timeline")
        timestamp_count = 0
        for segment in segments:
            decoded = segment.get("decoded_tokens", [])
            timestamps = segment.get("token_start_seconds")
            if not timestamps or len(timestamps) != len(decoded):
                raise RuntimeError("token timestamp cardinality mismatch")
            previous = segment["start_seconds"]
            for timestamp in timestamps:
                if (not isinstance(timestamp, (int, float)) or not math.isfinite(timestamp)
                        or timestamp < previous or timestamp < segment["start_seconds"]
                        or timestamp > segment["end_seconds"]):
                    raise RuntimeError("invalid token timestamp timeline")
                previous = timestamp
            timestamp_count += len(timestamps)
        digest = hashlib.sha256(result["text"].encode("utf-8")).hexdigest()[:12]
        silence = pinned_fixture(silence_fixture(), SILENCE_SHA256, "silence")
        noise = pinned_fixture(noise_fixture(), NOISE_SHA256, "noise")
        assert_empty_success(port, capability, silence, "silence")
        assert_empty_success(port, capability, noise, "noise")
        summary = f"sidecar smoke passed: segments={len(segments)} tokens={tokens} timestamps={timestamp_count} text_sha256={digest} empty_cases=2"
    finally:
        process.terminate()
        forced_kill = False
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
            forced_kill = True
        listener.close()
        stderr = process.stderr.read()
        if forced_kill or process.returncode not in (-15, 0):
            digest = hashlib.sha256(stderr).hexdigest()[:12]
            lines = stderr.count(b"\n") + bool(stderr and not stderr.endswith(b"\n"))
            raise RuntimeError(
                f"sidecar exited unexpectedly: code={process.returncode} "
                f"forced_kill={forced_kill} stderr_bytes={len(stderr)} "
                f"stderr_lines={lines} stderr_sha256={digest}"
            )
    print(summary)


if __name__ == "__main__":
    main()
