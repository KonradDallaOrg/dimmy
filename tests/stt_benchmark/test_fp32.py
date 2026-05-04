#!/usr/bin/env python3
"""Test Parakeet TDT v3 fp32 + Canary 1B v2 + Whisper-large-v3-turbo locally via onnx-asr."""
import onnx_asr
import time
import wave
import numpy as np

SAMPLES = [
    "/mnt/c/Users/konradd/AppData/Roaming/dimmy/audio_debug/2026-03-23_22-51-13/processed.wav",
    "/mnt/c/Users/konradd/AppData/Roaming/dimmy/audio_debug/2026-03-23_22-51-28/processed.wav",
]

def load_wav(path):
    with wave.open(path, "rb") as wf:
        sr = wf.getframerate()
        n = wf.getnframes()
        ch = wf.getnchannels()
        sw = wf.getsampwidth()
        raw = wf.readframes(n)
    if sw == 2:
        x = np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768.0
    else:
        x = np.frombuffer(raw, dtype=np.float32)
    if ch > 1:
        x = x.reshape(-1, ch).mean(axis=1)
    if sr != 16000:
        from librosa import resample
        x = resample(x, orig_sr=sr, target_sr=16000)
        sr = 16000
    return x, sr, len(x) / sr

def bench(model_id, path=None, label=None):
    label = label or model_id
    print(f"\n=== {label} ===")
    t0 = time.perf_counter()
    try:
        m = onnx_asr.load_model(model_id, path=path) if path else onnx_asr.load_model(model_id)
    except Exception as e:
        print(f"  load failed: {e}")
        return
    print(f"  loaded in {(time.perf_counter()-t0)*1000:.0f}ms")
    for sample in SAMPLES:
        x, sr, dur = load_wav(sample)
        # warm + timed
        for run in range(2):
            t1 = time.perf_counter()
            try:
                text = m.recognize(x)
            except Exception as e:
                print(f"  recognize failed: {e}")
                return
            elapsed = time.perf_counter() - t1
            tag = "warm" if run == 1 else "cold"
            short = sample.split("/")[-2]
            print(f"  [{tag}] {short} ({dur:.2f}s)  latency: {elapsed*1000:.0f}ms  → \"{text}\"")

# fp32 Parakeet using local files
bench("nemo-parakeet-tdt-0.6b-v3",
      path="/home/konrad/code/pai-voice/.scratch/parakeet-fp32",
      label="Parakeet TDT v3 FP32 (local files)")

# Canary 1B v2 (auto-download)
bench("nemo-canary-1b-v2", label="Canary 1B v2 (HF auto-download)")

# Whisper-large-v3-turbo via onnx-community (auto-download)
bench("onnx-community/whisper-large-v3-turbo", label="Whisper-large-v3-turbo ONNX (HF auto-download)")
