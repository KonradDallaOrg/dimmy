"""Quick FFI smoke for dimmy_compute_audio_peaks on a single file.

Bypasses the C# host so we can pinpoint whether the Rust decoder is the
one rejecting an .ogg that Audacity reads cleanly. Prints the FFI rc
plus the first slice of JSON when successful.

Usage: python scripts/dev/test-peaks.py <path-to-audio>
"""
import ctypes
import os
import sys
from pathlib import Path

DLL = Path(os.environ.get(
    "DIMMY_DLL",
    "C:/code/dimmy/platforms/windows/Dimmy.Windows/bin/x64/Debug/net8.0-windows10.0.19041.0/win-x64/dimmy_lib.dll",
))

def main(argv):
    if len(argv) < 2:
        print("usage: test-peaks.py <audio-path> [bucket_count=200]", file=sys.stderr)
        return 2
    audio = argv[1]
    buckets = int(argv[2]) if len(argv) > 2 else 200
    if not Path(audio).exists():
        print(f"FATAL: audio missing: {audio}", file=sys.stderr)
        return 2
    if not DLL.exists():
        print(f"FATAL: DLL missing: {DLL}", file=sys.stderr)
        return 2

    lib = ctypes.WinDLL(str(DLL))
    # int dimmy_compute_audio_peaks(const char*, int, char*, int)
    lib.dimmy_compute_audio_peaks.restype = ctypes.c_int
    lib.dimmy_compute_audio_peaks.argtypes = [
        ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_int
    ]

    # Same buffer-size heuristic the C# host uses.
    needed = buckets * 16 + 128
    bufLen = ((needed + 4095) // 4096) * 4096
    buf = ctypes.create_string_buffer(bufLen)

    rc = lib.dimmy_compute_audio_peaks(
        audio.encode("utf-8"), buckets, buf, bufLen,
    )
    print(f"rc = {rc}")
    if rc > 0:
        payload = buf.raw[:rc].decode("utf-8", errors="replace")
        print(f"json[:240] = {payload[:240]}")
        # Quick stat: non-zero peak count
        import json
        try:
            d = json.loads(payload)
            peaks = d.get("peaks", [])
            nonzero = sum(1 for p in peaks if p > 0.001)
            print(f"peaks total={len(peaks)} nonzero={nonzero} duration_secs={d.get('duration_secs')}")
        except Exception as e:
            print(f"json parse failed: {e}")
    else:
        # rc table: -1 bad input, -2 decode/empty samples, -3 buf too small
        meanings = {-1: "bad input", -2: "decode failed OR 0 samples",
                    -3: "out buffer too small"}
        print(f"  → {meanings.get(rc, 'unknown')}")
    return 0 if rc > 0 else 1

if __name__ == "__main__":
    sys.exit(main(sys.argv))
