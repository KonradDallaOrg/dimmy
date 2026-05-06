#!/usr/bin/env bash
# test_benchmark.sh — STT quality & duration benchmark across all providers
#
# Usage:
#   ./tests/test_benchmark.sh download          # Download & convert audio samples only
#   ./tests/test_benchmark.sh quick             # Short samples (5s-90s) — quality/WER
#   ./tests/test_benchmark.sh medium            # Medium samples (5-10min)
#   ./tests/test_benchmark.sh long              # Long samples (30-60min)
#   ./tests/test_benchmark.sh all               # All tiers
#   ./tests/test_benchmark.sh info              # Show provider limits & audio inventory
#
# Requires: curl, ffmpeg, jq, bc
# API keys: .env file with GROQ_KEY, OPENAI_KEY, DEEPGRAM_KEY, GEMINI_KEY
#
# Results are saved to tests/results/benchmark_YYYY-MM-DD_HHMMSS.md

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
AUDIO_DIR="$SCRIPT_DIR/audio"
RESULTS_DIR="$SCRIPT_DIR/results"
ENV_FILE="$PROJECT_DIR/.env"

MODE="${1:-info}"

# ── Audio sample definitions ─────────────────────────────────────────────────
# Format: id|duration|lang|description|url|transcript_excerpt
# Transcript excerpts are first ~100 chars for WER spot-check

declare -A SAMPLES_URL SAMPLES_DURATION SAMPLES_LANG SAMPLES_DESC SAMPLES_TRANSCRIPT SAMPLES_TIER

# --- Quick tier (5s - 90s) ---
SAMPLES_URL[jfk]="https://raw.githubusercontent.com/ggml-org/whisper.cpp/master/samples/jfk.wav"
SAMPLES_DURATION[jfk]="11s"
SAMPLES_LANG[jfk]="en"
SAMPLES_DESC[jfk]="JFK 'Ask not'"
SAMPLES_TRANSCRIPT[jfk]="And so my fellow Americans, ask not what your country can do for you, ask what you can do for your country."
SAMPLES_TIER[jfk]="quick"

SAMPLES_URL[micromachines]="https://cdn.openai.com/whisper/draft-20220913a/micro-machines.wav"
SAMPLES_DURATION[micromachines]="35s"
SAMPLES_LANG[micromachines]="en"
SAMPLES_DESC[micromachines]="Micro Machines Man (fast speech)"
SAMPLES_TRANSCRIPT[micromachines]="This is the Micro Machine Man presenting the most midget miniature motorcade of Micro Machines."
SAMPLES_TIER[micromachines]="quick"

SAMPLES_URL[gettysburg]="https://github.com/runpod-workers/sample-inputs/raw/main/audio/gettysburg.wav"
SAMPLES_DURATION[gettysburg]="90s"
SAMPLES_LANG[gettysburg]="en"
SAMPLES_DESC[gettysburg]="Gettysburg Address"
SAMPLES_TRANSCRIPT[gettysburg]="Four score and seven years ago our fathers brought forth on this continent, a new nation, conceived in Liberty"
SAMPLES_TIER[gettysburg]="quick"

SAMPLES_URL[harvard_f]="http://www.voiptroubleshooter.com/open_speech/american/OSR_us_000_0010_8k.wav"
SAMPLES_DURATION[harvard_f]="30s"
SAMPLES_LANG[harvard_f]="en"
SAMPLES_DESC[harvard_f]="Harvard Sentences (female, 8kHz)"
SAMPLES_TRANSCRIPT[harvard_f]="The birch canoe slid on the smooth planks."
SAMPLES_TIER[harvard_f]="quick"

SAMPLES_URL[harvard_m]="http://www.voiptroubleshooter.com/open_speech/american/OSR_us_000_0030_8k.wav"
SAMPLES_DURATION[harvard_m]="30s"
SAMPLES_LANG[harvard_m]="en"
SAMPLES_DESC[harvard_m]="Harvard Sentences (male, 8kHz)"
SAMPLES_TRANSCRIPT[harvard_m]="The boy was there when the sun rose."
SAMPLES_TIER[harvard_m]="quick"

# --- Medium tier (5-10 min) ---
SAMPLES_URL[pinocchio_en]="https://archive.org/download/pinocchio_1310_librivox/pinocchio_01_collodi_128kb.mp3"
SAMPLES_DURATION[pinocchio_en]="5min"
SAMPLES_LANG[pinocchio_en]="en"
SAMPLES_DESC[pinocchio_en]="Pinocchio Ch.1 (English)"
SAMPLES_TRANSCRIPT[pinocchio_en]="There was once upon a time a piece of wood."
SAMPLES_TIER[pinocchio_en]="medium"

SAMPLES_URL[two_cities]="https://archive.org/download/tale_two_cities_librivox/tale_of_two_cities_01_dickens.mp3"
SAMPLES_DURATION[two_cities]="7min"
SAMPLES_LANG[two_cities]="en"
SAMPLES_DESC[two_cities]="Tale of Two Cities Ch.1"
SAMPLES_TRANSCRIPT[two_cities]="It was the best of times, it was the worst of times"
SAMPLES_TIER[two_cities]="medium"

SAMPLES_URL[pride]="https://archive.org/download/pride_and_prejudice_librivox/prideandprejudice_07_austen.mp3"
SAMPLES_DURATION[pride]="11min"
SAMPLES_LANG[pride]="en"
SAMPLES_DESC[pride]="Pride & Prejudice Ch.7"
SAMPLES_TRANSCRIPT[pride]="Mr. Bennet's property consisted almost entirely in an estate"
SAMPLES_TIER[pride]="medium"

SAMPLES_URL[divina_10m]="https://archive.org/download/divina_commedia_librivox/divina_commedia_06_alighieri.mp3"
SAMPLES_DURATION[divina_10m]="12min"
SAMPLES_LANG[divina_10m]="it"
SAMPLES_DESC[divina_10m]="Divina Commedia Canti XXVI-XXX (Italian)"
SAMPLES_TRANSCRIPT[divina_10m]=""
SAMPLES_TIER[divina_10m]="medium"

# --- Long tier (30-60 min) ---
SAMPLES_URL[sherlock]="https://archive.org/download/adventures_sherlock_holmes_rg_librivox/adventuresholmes_01_doyle.mp3"
SAMPLES_DURATION[sherlock]="28min"
SAMPLES_LANG[sherlock]="en"
SAMPLES_DESC[sherlock]="Sherlock Holmes: Scandal in Bohemia"
SAMPLES_TRANSCRIPT[sherlock]="To Sherlock Holmes she is always the woman"
SAMPLES_TIER[sherlock]="long"

SAMPLES_URL[walden_30m]="https://archive.org/download/walden_librivox/walden_c01_p01.mp3"
SAMPLES_DURATION[walden_30m]="30min"
SAMPLES_LANG[walden_30m]="en"
SAMPLES_DESC[walden_30m]="Walden Ch.1 Pt.1"
SAMPLES_TRANSCRIPT[walden_30m]="When I wrote the following pages"
SAMPLES_TIER[walden_30m]="long"

SAMPLES_URL[divina_30m]="https://archive.org/download/divina_commedia_librivox/divina_commedia_04_alighieri.mp3"
SAMPLES_DURATION[divina_30m]="29min"
SAMPLES_LANG[divina_30m]="it"
SAMPLES_DESC[divina_30m]="Divina Commedia Canti XVI-XX (Italian)"
SAMPLES_TRANSCRIPT[divina_30m]=""
SAMPLES_TIER[divina_30m]="long"

SAMPLES_URL[walden_60m]="https://archive.org/download/walden_librivox/walden_c09.mp3"
SAMPLES_DURATION[walden_60m]="73min"
SAMPLES_LANG[walden_60m]="en"
SAMPLES_DESC[walden_60m]="Walden Ch.9 'The Ponds'"
SAMPLES_TRANSCRIPT[walden_60m]="A lake is the landscape's most beautiful and expressive feature"
SAMPLES_TIER[walden_60m]="long"

SAMPLES_URL[divina_60m]="https://archive.org/download/divina_commedia_librivox/divina_commedia_18_alighieri.mp3"
SAMPLES_DURATION[divina_60m]="68min"
SAMPLES_LANG[divina_60m]="it"
SAMPLES_DESC[divina_60m]="Divina Commedia Paradiso XXII-XXVII (Italian)"
SAMPLES_TRANSCRIPT[divina_60m]=""
SAMPLES_TIER[divina_60m]="long"

SAMPLES_URL[pinocchio_it]="https://archive.org/download/avventure_pinocchio_librivox/avventurepinocchio_01_collodi_64kb.mp3"
SAMPLES_DURATION[pinocchio_it]="5min"
SAMPLES_LANG[pinocchio_it]="it"
SAMPLES_DESC[pinocchio_it]="Pinocchio Cap.1 (Italian)"
SAMPLES_TRANSCRIPT[pinocchio_it]="C'era una volta"
SAMPLES_TIER[pinocchio_it]="medium"

# All sample IDs in order
ALL_SAMPLES=(jfk micromachines gettysburg harvard_f harvard_m pinocchio_en two_cities pride divina_10m pinocchio_it sherlock walden_30m divina_30m walden_60m divina_60m)

# ── STT provider definitions ─────────────────────────────────────────────────
# Format: name|model|key_var|format|url|max_file_mb|max_duration
STT_PROVIDERS=(
  "Groq|whisper-large-v3-turbo|GROQ_KEY|openai|https://api.groq.com/openai/v1/audio/transcriptions|25|unlimited"
  "Groq|whisper-large-v3|GROQ_KEY|openai|https://api.groq.com/openai/v1/audio/transcriptions|25|unlimited"
  "OpenAI|whisper-1|OPENAI_KEY|openai|https://api.openai.com/v1/audio/transcriptions|25|unlimited"
  "OpenAI|gpt-4o-transcribe|OPENAI_KEY|openai|https://api.openai.com/v1/audio/transcriptions|25|25min"
  "OpenAI|gpt-4o-mini-transcribe|OPENAI_KEY|openai|https://api.openai.com/v1/audio/transcriptions|25|25min"
  "Deepgram|nova-3|DEEPGRAM_KEY|deepgram|https://api.deepgram.com/v1/listen?model=nova-3&smart_format=true|2048|unlimited"
  "Gemini|gemini-2.5-flash|GEMINI_KEY|gemini|https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent|20|9.5hr"
  "Fireworks|whisper-v3-turbo|FIREWORKS_API_KEY|openai|https://audio-turbo.api.fireworks.ai/v1/audio/transcriptions|100|unlimited"
  "Fireworks|whisper-v3|FIREWORKS_API_KEY|openai|https://audio-prod.us-virginia-1.direct.fireworks.ai/v1/audio/transcriptions|100|unlimited"
  "Together|nvidia/parakeet-tdt-0.6b-v3|TOGETHER_API_KEY|openai|https://api.together.xyz/v1/audio/transcriptions|100|unlimited"
  "Together|openai/whisper-large-v3|TOGETHER_API_KEY|openai|https://api.together.xyz/v1/audio/transcriptions|100|unlimited"
  "Together|mistralai/Voxtral-Mini-3B-2507|TOGETHER_API_KEY|openai|https://api.together.xyz/v1/audio/transcriptions|100|unlimited"
)

# ── Helpers ───────────────────────────────────────────────────────────────────

check_deps() {
  local missing=()
  for cmd in curl ffmpeg jq bc; do
    command -v "$cmd" &>/dev/null || missing+=("$cmd")
  done
  if [[ ${#missing[@]} -gt 0 ]]; then
    echo "ERROR: Missing dependencies: ${missing[*]}" >&2
    echo "  Install: sudo apt install ${missing[*]}" >&2
    exit 1
  fi
}

load_env() {
  if [[ -f "$ENV_FILE" ]]; then
    set -a; source "$ENV_FILE"; set +a
  fi
}

# Download and convert a single audio sample to WAV 16kHz mono
download_sample() {
  local id="$1"
  local url="${SAMPLES_URL[$id]}"
  local raw_file="$AUDIO_DIR/raw/${id}.orig"
  local wav_file="$AUDIO_DIR/${id}.wav"

  if [[ -f "$wav_file" ]]; then
    return 0
  fi

  mkdir -p "$AUDIO_DIR/raw"

  echo -n "  Downloading $id (${SAMPLES_DURATION[$id]}, ${SAMPLES_LANG[$id]})... "
  if ! curl -sL --max-time 120 -o "$raw_file" "$url"; then
    echo "FAILED"
    return 1
  fi
  echo "OK"

  echo -n "  Converting to WAV 16kHz mono... "
  if ! ffmpeg -y -i "$raw_file" -ar 16000 -ac 1 -c:a pcm_s16le "$wav_file" -loglevel error 2>/dev/null; then
    echo "FAILED"
    return 1
  fi

  local size_kb
  size_kb=$(du -k "$wav_file" | cut -f1)
  local size_mb
  size_mb=$(echo "scale=1; $size_kb / 1024" | bc)
  echo "OK (${size_mb}MB)"
}

# Download all samples for a tier
download_tier() {
  local tier="$1"
  echo ""
  echo "  Downloading $tier tier audio samples..."
  echo ""
  for id in "${ALL_SAMPLES[@]}"; do
    if [[ "${SAMPLES_TIER[$id]}" == "$tier" ]]; then
      download_sample "$id" || true
    fi
  done
}

# Get WAV file size in MB
wav_size_mb() {
  local file="$1"
  local kb
  kb=$(du -k "$file" | cut -f1)
  echo "scale=1; $kb / 1024" | bc
}

# Get WAV duration in seconds
wav_duration_secs() {
  ffprobe -v error -show_entries format=duration -of csv=p=0 "$1" 2>/dev/null | cut -d. -f1
}

# Simple word overlap score (poor man's WER)
# Returns percentage of reference words found in hypothesis
word_overlap() {
  local ref="$1" hyp="$2"
  if [[ -z "$ref" || -z "$hyp" ]]; then
    echo "-"
    return
  fi
  # Lowercase, remove punctuation, split to words
  local ref_words hyp_words
  ref_words=$(echo "$ref" | tr '[:upper:]' '[:lower:]' | tr -d '[:punct:]' | tr -s ' ')
  hyp_words=$(echo "$hyp" | tr '[:upper:]' '[:lower:]' | tr -d '[:punct:]' | tr -s ' ')

  local total=0 found=0
  for word in $ref_words; do
    total=$((total + 1))
    if echo " $hyp_words " | grep -qi " $word "; then
      found=$((found + 1))
    fi
  done

  if [[ $total -eq 0 ]]; then
    echo "-"
  else
    echo "scale=0; $found * 100 / $total" | bc
  fi
}

# Send audio to STT provider, return transcript text
call_stt() {
  local wav_file="$1" lang="$2" prov_entry="$3"
  IFS='|' read -r prov model key_var fmt url max_mb max_dur <<< "$prov_entry"
  local key="${!key_var:-}"

  if [[ -z "$key" ]]; then
    echo "SKIP:No $key_var"
    return
  fi

  # Check file size against provider limit
  local size_mb
  size_mb=$(wav_size_mb "$wav_file")
  if (( $(echo "$size_mb > $max_mb" | bc -l) )); then
    echo "SKIP:File ${size_mb}MB > ${max_mb}MB limit"
    return
  fi

  local resp text
  local WAV_DATA

  case "$fmt" in
    openai)
      resp=$(curl -sS --max-time 300 "$url" \
        -H "Authorization: Bearer $key" \
        -F "file=@$wav_file" \
        -F "model=$model" \
        -F "response_format=json" \
        -F "language=$lang" 2>&1) || { echo "ERROR:curl failed"; return; }
      text=$(echo "$resp" | jq -r '.text // empty' 2>/dev/null)
      ;;
    deepgram)
      resp=$(curl -sS --max-time 300 "${url}&language=$lang" \
        -H "Authorization: Token $key" \
        -H "Content-Type: audio/wav" \
        --data-binary "@$wav_file" 2>&1) || { echo "ERROR:curl failed"; return; }
      text=$(echo "$resp" | jq -r '.results.channels[0].alternatives[0].transcript // empty' 2>/dev/null)
      ;;
    gemini)
      # Pipe base64 via stdin to avoid ARG_MAX on large files
      local body_file
      body_file=$(mktemp)
      (base64 -w0 "$wav_file" 2>/dev/null || base64 "$wav_file" 2>/dev/null) | \
        jq -Rs --arg lang "$lang" '{
          contents: [{
            parts: [
              { text: ("Transcribe this audio exactly as spoken. Output ONLY the transcribed text. Language: " + $lang) },
              { inline_data: { mime_type: "audio/wav", data: . } }
            ]
          }],
          generationConfig: { maxOutputTokens: 8192 }
        }' > "$body_file"
      resp=$(curl -sS --max-time 300 "$url" \
        -H "x-goog-api-key: $key" \
        -H "Content-Type: application/json" \
        -d @"$body_file" 2>&1) || { rm -f "$body_file"; echo "ERROR:curl failed"; return; }
      rm -f "$body_file"
      text=$(echo "$resp" | jq -r '.candidates[0].content.parts[0].text // empty' 2>/dev/null)
      ;;
  esac

  local err_msg
  err_msg=$(echo "$resp" | jq -r '.error.message // empty' 2>/dev/null)
  if [[ -n "$err_msg" ]]; then
    echo "ERROR:$err_msg"
    return
  fi

  echo "$text"
}

# Truncate text for display
truncate_text() {
  local max="$1" text
  text=$(echo "$2" | tr '\n' ' ' | sed 's/  */ /g')
  [[ ${#text} -gt $max ]] && echo "${text:0:$((max-3))}..." || echo "$text"
}

# ── Commands ──────────────────────────────────────────────────────────────────

cmd_download() {
  check_deps
  echo ""
  echo "  Downloading all audio samples to $AUDIO_DIR"
  download_tier "quick"
  download_tier "medium"
  download_tier "long"
  echo ""
  echo "  Audio inventory:"
  cmd_inventory
}

cmd_inventory() {
  echo ""
  printf "  %-15s %-8s %-4s %-40s %s\n" "ID" "Duration" "Lang" "Description" "WAV Size"
  printf "  %-15s %-8s %-4s %-40s %s\n" "───────────────" "────────" "────" "────────────────────────────────────────" "────────"
  for id in "${ALL_SAMPLES[@]}"; do
    local wav_file="$AUDIO_DIR/${id}.wav"
    local size="-"
    if [[ -f "$wav_file" ]]; then
      size="$(wav_size_mb "$wav_file")MB"
    fi
    printf "  %-15s %-8s %-4s %-40s %s\n" "$id" "${SAMPLES_DURATION[$id]}" "${SAMPLES_LANG[$id]}" "${SAMPLES_DESC[$id]}" "$size"
  done
  echo ""
}

cmd_info() {
  echo ""
  echo "  ┌─────────────────────────────────────────────────────────────────┐"
  echo "  │  STT BENCHMARK — Provider Limits & Audio Inventory             │"
  echo "  └─────────────────────────────────────────────────────────────────┘"
  echo ""
  echo "  Provider Limits:"
  printf "  %-12s %-25s %-10s %-12s %s\n" "Provider" "Model" "Max File" "Max Duration" "Key"
  printf "  %-12s %-25s %-10s %-12s %s\n" "────────────" "─────────────────────────" "──────────" "────────────" "───"
  for entry in "${STT_PROVIDERS[@]}"; do
    IFS='|' read -r prov model key_var fmt url max_mb max_dur <<< "$entry"
    local key="${!key_var:-}"
    local key_status="SET"
    [[ -z "$key" ]] && key_status="MISSING"
    printf "  %-12s %-25s %-10s %-12s %s\n" "$prov" "$model" "${max_mb}MB" "$max_dur" "$key_status"
  done

  cmd_inventory

  echo "  Commands:"
  echo "    ./tests/test_benchmark.sh download   # Download all audio samples"
  echo "    ./tests/test_benchmark.sh quick       # Run short samples (5s-90s)"
  echo "    ./tests/test_benchmark.sh medium      # Run medium samples (5-10min)"
  echo "    ./tests/test_benchmark.sh long        # Run long samples (30-60min)"
  echo "    ./tests/test_benchmark.sh all         # Run all tiers"
  echo ""
}

run_tier() {
  local tier="$1"
  local timestamp
  timestamp=$(date +"%Y-%m-%d_%H%M%S")
  mkdir -p "$RESULTS_DIR"
  local result_file="$RESULTS_DIR/benchmark_${tier}_${timestamp}.md"

  echo ""
  echo "  Running $tier tier benchmark..."
  echo ""

  # Ensure audio is downloaded
  download_tier "$tier"

  # Collect sample IDs for this tier
  local tier_samples=()
  for id in "${ALL_SAMPLES[@]}"; do
    if [[ "${SAMPLES_TIER[$id]}" == "$tier" ]]; then
      local wav="$AUDIO_DIR/${id}.wav"
      if [[ -f "$wav" ]]; then
        tier_samples+=("$id")
      else
        echo "  WARNING: $id.wav not found, skipping"
      fi
    fi
  done

  if [[ ${#tier_samples[@]} -eq 0 ]]; then
    echo "  No audio files available for $tier tier"
    return
  fi

  # Start markdown output
  {
    echo "# STT Benchmark — ${tier^} Tier"
    echo ""
    echo "Date: $(date '+%Y-%m-%d %H:%M:%S')"
    echo ""
    echo "## Audio Samples"
    echo ""
    echo "| ID | Duration | Lang | Description | WAV Size |"
    echo "|---|---|---|---|---|"
    for id in "${tier_samples[@]}"; do
      local wav="$AUDIO_DIR/${id}.wav"
      local size
      size=$(wav_size_mb "$wav")
      local dur
      dur=$(wav_duration_secs "$wav")
      echo "| $id | ${dur}s | ${SAMPLES_LANG[$id]} | ${SAMPLES_DESC[$id]} | ${size}MB |"
    done
    echo ""
    echo "## Results"
    echo ""
    echo "| Sample | Provider | Model | Latency | Words | Match% | First 80 chars |"
    echo "|---|---|---|---|---|---|---|"
  } > "$result_file"

  # Run each sample against each provider
  for id in "${tier_samples[@]}"; do
    local wav="$AUDIO_DIR/${id}.wav"
    local lang="${SAMPLES_LANG[$id]}"
    local ref="${SAMPLES_TRANSCRIPT[$id]}"

    echo "  Testing: $id (${SAMPLES_DURATION[$id]}, ${SAMPLES_LANG[$id]})"

    for prov_entry in "${STT_PROVIDERS[@]}"; do
      IFS='|' read -r prov model key_var fmt url max_mb max_dur <<< "$prov_entry"
      echo -n "    $prov/$model... "

      local start_ns end_ns ms result word_count overlap preview
      start_ns=$(date +%s%N)
      result=$(call_stt "$wav" "$lang" "$prov_entry")
      end_ns=$(date +%s%N)
      ms=$(( (end_ns - start_ns) / 1000000 ))

      if [[ "$result" == SKIP:* || "$result" == ERROR:* ]]; then
        echo "${result#*:}"
        echo "| $id | $prov | $model | - | - | - | ${result#*:} |" >> "$result_file"
      else
        word_count=$(echo "$result" | wc -w | tr -d ' ')
        overlap=$(word_overlap "$ref" "$result")
        preview=$(truncate_text 80 "$result")
        echo "${ms}ms, ${word_count} words, match=${overlap}%"
        echo "| $id | $prov | $model | ${ms}ms | $word_count | ${overlap}% | $preview |" >> "$result_file"
      fi
    done
    echo ""
  done

  echo "" >> "$result_file"
  echo "---" >> "$result_file"
  echo "*Generated by Dimmy test_benchmark.sh*" >> "$result_file"

  echo "  Results saved to: $result_file"
  echo ""
}

# ── Main ──────────────────────────────────────────────────────────────────────

load_env

case "$MODE" in
  download) check_deps; cmd_download ;;
  info)     load_env; cmd_info ;;
  quick)    check_deps; run_tier "quick" ;;
  medium)   check_deps; run_tier "medium" ;;
  long)     check_deps; run_tier "long" ;;
  all)      check_deps; run_tier "quick"; run_tier "medium"; run_tier "long" ;;
  *)        echo "Usage: $0 [download|info|quick|medium|long|all]"; exit 1 ;;
esac
