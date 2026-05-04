#!/usr/bin/env python3
"""Simula streaming dictation: chunk Parakeet da N secondi, misura latenza per chunk."""
import onnx_asr
import time
import wave
import numpy as np

LONG = "/mnt/c/Users/konradd/AppData/Roaming/dimmy/audio_debug/2026-03-27_09-33-17/processed.wav"

print("loading Parakeet fp32...")
m = onnx_asr.load_model("nemo-parakeet-tdt-0.6b-v3",
                        path="/home/konrad/code/pai-voice/.scratch/parakeet-fp32")
print("loaded.\n")

with wave.open(LONG, "rb") as wf:
    sr = wf.getframerate()
    raw = wf.readframes(wf.getnframes())
samples = np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768.0
total_dur = len(samples) / sr
print(f"audio: sr={sr}, duration={total_dur:.1f}s, samples={len(samples)}\n")

# Warmup
print("warmup (1s clip)...")
_ = m.recognize(samples[:sr])
print("warmup done.\n")

for chunk_secs in [3, 5, 10]:
    print(f"=== chunk={chunk_secs}s ===")
    chunk_samples = chunk_secs * sr
    chunks = [samples[i:i+chunk_samples] for i in range(0, len(samples), chunk_samples)]
    full_text = []
    latencies = []
    total_start = time.perf_counter()
    for i, chunk in enumerate(chunks):
        if len(chunk) < sr * 0.5:  # skip <0.5s residual
            continue
        t0 = time.perf_counter()
        text = m.recognize(chunk)
        elapsed = time.perf_counter() - t0
        latencies.append(elapsed)
        full_text.append(text)
        chunk_dur = len(chunk) / sr
        rt = chunk_dur / elapsed if elapsed > 0 else 0
        worst = "⚠️ SLOW" if elapsed > chunk_dur else "✓"
        if i < 3 or i >= len(chunks) - 2:
            print(f"  chunk {i+1:2d}/{len(chunks)} ({chunk_dur:.1f}s): {elapsed*1000:5.0f}ms ({rt:.1f}x rt) {worst}  → \"{text[:60]}{'...' if len(text)>60 else ''}\"")
    total_elapsed = time.perf_counter() - total_start
    avg = sum(latencies) / len(latencies)
    mx = max(latencies)
    print(f"  -- total: {total_elapsed:.2f}s, avg/chunk: {avg*1000:.0f}ms, max/chunk: {mx*1000:.0f}ms")
    print(f"  -- realtime ratio (audio/processing): {total_dur/total_elapsed:.1f}x")
    real_time_safe = mx < chunk_secs
    print(f"  -- CPU real-time safe? {'YES ✓' if real_time_safe else 'NO ❌'} (max {mx*1000:.0f}ms vs {chunk_secs*1000}ms budget)")
    combined = " ".join(full_text)
    print(f"  -- combined output ({len(combined)} chars): \"{combined[:200]}...\"")
    print()
