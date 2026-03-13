# Dimmy — STT Provider Benchmark

> Benchmark run: 2026-03-13 | Dimmy v0.3.46 | 7 providers, 10 audio samples

## Quick Tier (5s - 90s)

| Sample | Duration | Lang | Groq Turbo | Groq v3 | OpenAI whisper-1 | OpenAI 4o-transcribe | OpenAI 4o-mini | Deepgram Nova-3 | Gemini 2.5 Flash |
|--------|----------|------|-----------|---------|-----------------|---------------------|---------------|----------------|-----------------|
| JFK "Ask not" | 11s | EN | **815ms** 100% | 832ms 100% | 1834ms 100% | 1173ms 100% | 1188ms 100% | 2227ms 100% | 1569ms 100% |
| Micro Machines (fast speech) | 35s | EN | **838ms** 100% | 1088ms 100% | 3545ms 73% | 5161ms 100% | 2211ms 100% | 3304ms 93% | 2689ms 93% |
| Gettysburg Address | 90s | EN | 823ms 100% | **716ms** 100% | 1875ms 100% | 1563ms 100% | 1245ms 100% | 4008ms 100% | 1669ms 89% |
| Harvard Sentences (F) | 30s | EN | **902ms** 100% | 943ms 100% | 1849ms 100% | 2349ms 100% | 1781ms 100% | 4390ms 100% | 2087ms 100% |
| Harvard Sentences (M) | 30s | EN | **736ms** 100% | 800ms 100% | 2542ms 100% | 4699ms 100% | 3239ms 100% | 6079ms 100% | 2580ms 100% |

## Medium Tier (5min - 12min)

| Sample | Duration | Lang | Groq Turbo | Groq v3 | OpenAI whisper-1 | OpenAI 4o-transcribe | OpenAI 4o-mini | Deepgram Nova-3 | Gemini 2.5 Flash |
|--------|----------|------|-----------|---------|-----------------|---------------------|---------------|----------------|-----------------|
| Pinocchio Ch.1 | 5min | EN | **1.5s** 100% | 1.5s 100% | 15s 100% | 14s 100% | 10s 100% | 19s 100% | 11s 100% |
| Tale of Two Cities Ch.1 | 7min | EN | **1.7s** 100% | 2.1s 100% | 22s 100% | 21s 100% | 15s 100% | 26s 100% | 10s 100% |
| Pride & Prejudice Ch.7 | 11min | EN | 4.6s 100% | **3.3s** 100% | 34s 100% | 31s 100% | 25s 100% | 66s 77% | 15s 100% |
| Divina Commedia (Inferno) | 12min | IT | LIMIT 25MB | LIMIT 25MB | LIMIT 25MB | LIMIT 25MB | LIMIT 25MB | 249s 100% | LIMIT 20MB |
| Pinocchio Cap.1 | 5min | IT | **1.7s** 100% | 2.1s 100% | 17s 100% | 20s 100% | 14s 100% | 41s 100% | 8s 100% |

## Provider Comparison

### Speed (latency for 5-11min audio)

| Provider | 5min EN | 7min EN | 11min EN | 5min IT |
|----------|---------|---------|----------|---------|
| **Groq Turbo** | 1.5s | 1.7s | 4.6s | 1.7s |
| **Groq v3** | 1.5s | 2.1s | 3.3s | 2.1s |
| **Gemini 2.5 Flash** | 11s | 10s | 15s | 8s |
| **OpenAI 4o-mini** | 10s | 15s | 25s | 14s |
| **OpenAI 4o-transcribe** | 14s | 21s | 31s | 20s |
| **OpenAI whisper-1** | 15s | 22s | 34s | 17s |
| **Deepgram Nova-3** | 19s | 26s | 66s | 41s |

### File Size Limits

| Provider | Max File | Max Duration | 30min WAV (56MB) |
|----------|----------|-------------|------------------|
| Groq | 25 MB | - | Needs chunking (3 chunks) |
| OpenAI | 25 MB | 25 min (4o-transcribe) | Needs chunking (3 chunks) |
| Gemini (inline) | 20 MB | - | Needs chunking (3 chunks) |
| Deepgram | 2 GB | - | Single request OK |

### Key Findings

- **Groq is 10-20x faster** than every other provider. 11 minutes of audio transcribed in 3.3 seconds.
- **Gemini 2.5 Flash** is the second fastest and has excellent accuracy (100% on all medium tier).
- **OpenAI whisper-1** struggles with fast speech (73% on Micro Machines). The newer 4o-transcribe models fix this.
- **Deepgram** is the slowest but handles the largest files natively (2GB). Accuracy drops on longer audio (77% on 11min Pride & Prejudice).
- **Italian works great** across all providers. Groq and Gemini are the fastest for IT.
- **divina_10m (76.8MB WAV)** demonstrates the chunking problem: only Deepgram can handle it today. With Dimmy's new auto-chunking, all providers will work by splitting into ~10min chunks.

## Test Audio Sources

All samples are public domain (LibriVox) or freely available:

| ID | Source | License |
|----|--------|---------|
| jfk | [whisper.cpp samples](https://github.com/ggml-org/whisper.cpp) | Public domain |
| micromachines | [OpenAI Whisper demo](https://cdn.openai.com/whisper/) | Fair use |
| gettysburg | [runpod sample-inputs](https://github.com/runpod-workers/sample-inputs) | Public domain |
| harvard_f/m | [Open Speech Repository](https://www.voiptroubleshooter.com/open_speech/) | Public domain |
| pinocchio_en/it | [LibriVox](https://librivox.org/) | Public domain |
| two_cities | [LibriVox](https://librivox.org/) | Public domain |
| pride | [LibriVox](https://librivox.org/) | Public domain |
| divina_10m | [LibriVox](https://librivox.org/) | Public domain |

---
*Generated with [Dimmy](https://github.com/KonradDallaOrg/dimmy) test_benchmark.sh*
