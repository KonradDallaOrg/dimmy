#!/usr/bin/env python3
"""Test Parakeet TDT v3 locally via sherpa-onnx."""
import sherpa_onnx
import time
import wave
import numpy as np
import sys

MODEL_DIR = "/home/konrad/code/pai-voice/.scratch/parakeet/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"

print("loading model...")
t0 = time.perf_counter()
recognizer = sherpa_onnx.OfflineRecognizer.from_transducer(
    encoder=f"{MODEL_DIR}/encoder.int8.onnx",
    decoder=f"{MODEL_DIR}/decoder.int8.onnx",
    joiner=f"{MODEL_DIR}/joiner.int8.onnx",
    tokens=f"{MODEL_DIR}/tokens.txt",
    num_threads=4,
    decoding_method="modified_beam_search",
    max_active_paths=4,
    model_type="nemo_transducer",
)
print(f"  loaded in {(time.perf_counter()-t0)*1000:.0f}ms")

samples_to_test = [
    "/mnt/c/Users/konradd/AppData/Roaming/dimmy/audio_debug/2026-03-23_22-51-13/processed.wav",
    "/mnt/c/Users/konradd/AppData/Roaming/dimmy/audio_debug/2026-03-23_22-51-28/processed.wav",
]

for wav_path in samples_to_test:
    with wave.open(wav_path, "rb") as wf:
        sr = wf.getframerate()
        n = wf.getnframes()
        ch = wf.getnchannels()
        sw = wf.getsampwidth()
        raw = wf.readframes(n)

    if sw == 2:
        samples = np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768.0
    elif sw == 4:
        samples = np.frombuffer(raw, dtype=np.float32)
    else:
        print(f"unsupported sample width: {sw}")
        continue

    if ch > 1:
        samples = samples.reshape(-1, ch).mean(axis=1)

    duration = len(samples) / sr
    print(f"\n=== {wav_path.split('/')[-2]} ===")
    print(f"  sr={sr}, samples={len(samples)}, duration={duration:.2f}s")

    # warm + measure
    for run in range(2):
        t0 = time.perf_counter()
        stream = recognizer.create_stream()
        stream.accept_waveform(sr, samples)
        recognizer.decode_stream(stream)
        elapsed = time.perf_counter() - t0
        label = "warm" if run == 1 else "cold"
        print(f"  [{label}] latency: {elapsed*1000:.0f}ms  text: \"{stream.result.text}\"")
