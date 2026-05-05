# Handoff — Parakeet TDT v3 FP32 validation on Windows native

> Da: sessione WSL Linux (Konrad + Claude), 2026-05-05.
> A: sessione Claude su `C:\code\pai-voice` (Windows native + Visual Studio).
> Branch: `feat/stt-providers-expansion` (push fatto, fetch + checkout dovrebbe già funzionare).

## Cosa abbiamo già validato (WSL)

Su CPU Linux WSL, Parakeet TDT v3 FP32 con chunking 30 s + overlap 500 ms
+ dedup last-3-words processa **272 minuti totali di audio in 31 minuti
wall-clock (8.7× realtime)** con 100 % di match su 7/9 fixture
ground-truth e zero OOM anche su Walden 73 min. Numeri completi in
[`docs/dev/stt-benchmark-parakeet-local-2026-05-05.md`](../../dev/stt-benchmark-parakeet-local-2026-05-05.md).

Verdict WSL: la pipeline chunked-30s/dedup last-3 è **valida architetturalmente**
per Phase 4 (sostituzione/affiancamento di whisper.cpp con Parakeet via
ONNX Runtime). Manca un dato critico per chiudere il design: **quanto
guadagniamo su GPU Win**? L'aspettativa documentata è 3-5× ulteriore
speedup, ma è da misurare per chiudere il caso.

## Obiettivo di questa sessione Win (1-2 ore)

Misurare 3 numeri concreti su Win nativo con i sample audio già in
repo, e popolare un MD di confronto:

1. **CPU baseline su Win** — stesso script Python, stesso bundle FP32,
   ma su CPU Win nativa (non WSL emulato). Aspettativa: simile o
   leggermente meglio del WSL (no WSL2 overhead).
2. **GPU speedup con DirectML** — `providers=['DmlExecutionProvider']`.
   Funziona su qualsiasi GPU (NVIDIA, AMD, Intel, anche integrate).
   Aspettativa principale del run: **3-5× speedup vs CPU**.
3. **GPU speedup con CUDA** (solo se la macchina ha NVIDIA con driver
   CUDA pronto) — `providers=['CUDAExecutionProvider']`. Aspettativa:
   stesso ordine di DirectML o leggermente meglio.

Output atteso: un terzo MD `docs/dev/stt-benchmark-parakeet-win-2026-05-05.md`
con la stessa tabella delle 15 fixture × 3 backend (CPU, DirectML, CUDA),
così il confronto totale CPU-WSL / CPU-Win / DML / CUDA è in un unico file.

## Setup veloce (Win, da PowerShell)

```powershell
cd C:\code\pai-voice
git fetch origin
git checkout feat/stt-providers-expansion
git pull origin feat/stt-providers-expansion

# Python deps. Se hai Python 3.11/3.12 in PATH:
python -m pip install --upgrade pip
python -m pip install onnx-asr librosa soundfile

# GPU runtimes (provane uno o entrambi a seconda della GPU):
python -m pip install onnxruntime-directml      # AMD/Intel/NVIDIA via DirectML
python -m pip install onnxruntime-gpu           # NVIDIA via CUDA (se presente)
```

NB: `onnxruntime` (CPU), `onnxruntime-directml` e `onnxruntime-gpu` sono
**pacchetti pip mutuamente esclusivi**. Se installi più di uno, l'ultimo
vince. Per il bench, lavora **un EP alla volta**: installa, run, disinstalla,
prossimo. Oppure ambienti virtuali separati (più pulito):

```powershell
python -m venv .venv-cpu
.venv-cpu\Scripts\Activate.ps1
pip install onnx-asr librosa soundfile onnxruntime  # CPU only

# nuovo terminal/sessione:
python -m venv .venv-dml
.venv-dml\Scripts\Activate.ps1
pip install onnx-asr librosa soundfile onnxruntime-directml

# nuovo terminal/sessione:
python -m venv .venv-cuda
.venv-cuda\Scripts\Activate.ps1
pip install onnx-asr librosa soundfile onnxruntime-gpu
```

## Audio fixture

Lo script `tests/test_benchmark.sh` (bash) scarica 15 file audio da
LibriVox/whisper.cpp/openai. Su Win senza bash puoi:

- usare WSL solo per il download iniziale, copiare `tests/audio/*` nel
  Win clone, oppure
- runnare lo script via Git Bash, oppure
- scaricare direttamente i 15 file dal sh (tutti gli URL sono in
  `tests/test_benchmark.sh` SAMPLES_URL[id]).

Il primo lancio del benchmark Python richiede inoltre il bundle FP32
(2.5 GB). Due opzioni:

1. **Lascia `onnx-asr` scaricarlo automaticamente** dalla cache HF di
   Win (`%USERPROFILE%\.cache\huggingface\`). Modifica lo script: rimuovi
   `--model-path` o pass `--model-path ""` (alcune versioni di onnx-asr
   accettano stringa vuota; altrimenti il `path=` può essere lasciato
   senza parametro).
2. **Scarica una volta** via WSL (è già in `~/code/pai-voice/.scratch/parakeet-fp32/`)
   e copia in `C:\code\pai-voice\.scratch\parakeet-fp32\`.

## Run del bench

Lo script Python è già committato in
[`tests/stt_benchmark/run_parakeet_bench.py`](../../../tests/stt_benchmark/run_parakeet_bench.py).
Accetta i parametri:

```text
--audio-dir         path con i .wav (default tests/audio/)
--model-path        path al bundle FP32 ONNX (default .scratch/parakeet-fp32/)
--tier              quick | medium | long | all (default quick)
--chunk-secs        N    (0 = full transcribe; usa 30 per validare il pattern)
--overlap-ms        N    (default 500)
--output            path del MD di output (- = stdout)
```

**Una piccola modifica serve** prima del run su Win: lo script attualmente
forza `providers` non passa l'execution-provider preferito. Per
testare DirectML/CUDA, edita `run_parakeet_bench.py` riga ~94 dove c'è:

```python
m = onnx_asr.load_model("nemo-parakeet-tdt-0.6b-v3", path=args.model_path)
```

→ cambia in:

```python
m = onnx_asr.load_model(
    "nemo-parakeet-tdt-0.6b-v3",
    path=args.model_path,
    providers=['DmlExecutionProvider', 'CPUExecutionProvider'],   # DirectML
    # providers=['CUDAExecutionProvider', 'CPUExecutionProvider'], # CUDA
    # providers=['CPUExecutionProvider'],                          # CPU only
)
```

(Lascia `CPUExecutionProvider` come fallback in fondo alla lista, è
buona pratica ONNX Runtime: se il provider preferito fallisce su uno
specifico operatore, ritorna a CPU senza crashare).

Dopo l'edit, run:

```powershell
# CPU baseline su Win
python tests\stt_benchmark\run_parakeet_bench.py `
  --tier all --chunk-secs 30 --overlap-ms 500 `
  --output tests\results\benchmark_parakeet_win_cpu_2026-05-05.md

# DirectML (riedita providers=, riattiva venv-dml)
python tests\stt_benchmark\run_parakeet_bench.py `
  --tier all --chunk-secs 30 --overlap-ms 500 `
  --output tests\results\benchmark_parakeet_win_dml_2026-05-05.md

# CUDA (riedita providers=, riattiva venv-cuda) — solo se NVIDIA presente
python tests\stt_benchmark\run_parakeet_bench.py `
  --tier all --chunk-secs 30 --overlap-ms 500 `
  --output tests\results\benchmark_parakeet_win_cuda_2026-05-05.md
```

Tempi attesi (basandosi su WSL CPU):

- CPU Win: 25-35 min wall per `--tier all` (15 sample, 272 min audio totali)
- DirectML / CUDA: 5-12 min wall (3-5× speedup atteso)

## Cose da popolare nel MD finale

```markdown
# Parakeet TDT v3 FP32 — Windows-native benchmark

## Hardware

- CPU model: ____ (es. Intel i7-12700H, AMD Ryzen 7 7840U)
- GPU model: ____
- RAM totale: ____ GB
- Driver GPU: ____ (data + versione)

## Backend tested

| Backend | Wall-clock /tier all | Best chunk lat. | Worst chunk lat. | Note |
|---|---|---|---|---|
| CPU (onnxruntime) | __ min | __ ms | __ ms | baseline |
| DirectML (onnxruntime-directml) | __ min | __ ms | __ ms | __× speedup |
| CUDA (onnxruntime-gpu) | __ min | __ ms | __ ms | (se applicabile) |

[copia qui le tabelle quick/medium/long generate dallo script,
una sezione per backend]

## Verdict

[Compila in base a quello che vedi. Se DirectML o CUDA arriva sub-200ms
warm sui clip brevi → Phase 4 è promosso a "absolutely worth it".
Se sub-100ms → unblock Phase 5 streaming chunked seriamente. Se rimani
CPU-bound → discutiamo strategia diversa.]
```

## Phase 3 spike (separato, opzionale, dopo questo benchmark)

Se i numeri Win confermano lo speedup atteso, Phase 3 è il prossimo
gate: provare a buildare un binario Rust minimal con `sherpa-rs`
(crate Rust che wrappa sherpa-onnx) o direttamente con `ort` (crate
Rust ufficiale ONNX Runtime). Verifica che linki su MSVC + scala a
runtime con DirectML/CUDA EP. Output atteso: yes/no per ogni
piattaforma + 1 numero di latenza.

`sherpa-rs` è la strada più veloce (libreria già wrappata ma INT8
oriented). `ort` è più flessibile (FP32 + tutti gli execution provider)
ma richiede più boilerplate per integrare.

Tutti questi step sono parte di Phase 4 della
[`docs/dev/stt-providers-roadmap.md`](../../dev/stt-providers-roadmap.md).

## Note importanti

- **Niente push su `feat/licensing-poc`**. Tutti i commit di test/bench
  vanno su `feat/stt-providers-expansion` (questo branch).
- I MD/risultati che generi vanno committati. Il bundle FP32 in
  `.scratch/` è gitignored.
- Se trovi che lo script Python ha bug platform-specific (path
  separator, encoding), fixali nel file e committa — semplifica le
  prossime sessioni.

## Quando hai finito

Aggiungi una sezione `## Note dalla sessione Win` in fondo a questo
file con:

- I numeri reali (CPU/DirectML/CUDA per i 3 tier)
- Quale GPU/CPU hai usato
- Eventuali bug/sorprese dell'EP
- Verdict tuo: **vale la pena Phase 4** sì/no/forse
