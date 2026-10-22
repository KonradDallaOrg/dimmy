//! Streaming file load: decode, filter and cut an audio file into chunks
//! without ever holding the whole file in memory.
//!
//! WHY: file load used to decode the entire file into one `Vec<f32>`
//! (interleaved, so stereo doubled it), copy it to mono, copy it again
//! through the highpass and copy every chunk out before transcribing the
//! first. A 4-hour stereo m4a at 48 kHz peaked around 8-11 GB and froze
//! the machine. Here a file of any length costs one decoded packet plus
//! one chunk of audio (~6 MB at 48 kHz for the 30 s local chunks).
//!
//! Every stage matches the whole-file path it replaces, sample for sample:
//! - mono downmix: the same per-frame sum / channels, carried across blocks;
//! - highpass: `preprocess::FileLoadHighpass` keeps its state between blocks;
//! - chunk ends: `audio::first_silence_split` looks only at the first
//!   `max_chunk_samples` of what it is given, so a buffer holding just over
//!   one chunk cuts where `split_at_silence` on the whole file does.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Mono f32 blocks from a WAV (hound) or any Symphonia-supported file.
pub struct FileAudioStream {
    source: Source,
    pub sample_rate: u32,
    /// Frames per channel the container declares, when it does. Drives the
    /// progress bar; the bytes read stand in for it when it is missing.
    pub total_frames: Option<u64>,
    file_len: u64,
    bytes_read: Arc<AtomicU64>,
    channels: usize,
    /// Running sum of the frame being assembled and how many of its
    /// channels have arrived. A frame can straddle two blocks.
    frame_acc: f32,
    frame_fill: usize,
    frames_out: u64,
    done: bool,
}

enum Source {
    Wav(hound::WavReader<BufReader<CountingFile>>),
    Symphonia(Box<SymphoniaSource>),
}

struct SymphoniaSource {
    format: Box<dyn symphonia::core::formats::FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track_id: u32,
    sample_buf: Option<symphonia::core::audio::SampleBuffer<f32>>,
    consecutive_packet_errs: u32,
}

/// The file, counting the bytes read so progress works for containers
/// that do not declare their length in frames.
struct CountingFile {
    file: File,
    pos: Arc<AtomicU64>,
    len: u64,
}

impl Read for CountingFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.file.read(buf)?;
        self.pos.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

impl Seek for CountingFile {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let p = self.file.seek(to)?;
        self.pos.store(p, Ordering::Relaxed);
        Ok(p)
    }
}

impl symphonia::core::io::MediaSource for CountingFile {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}

/// WAV samples pulled per `read_block` call: ~85 ms of stereo at 48 kHz.
const WAV_BLOCK_SAMPLES: usize = 8192;

impl FileAudioStream {
    /// Open `path`: WAV through hound (same fast path as before), everything
    /// else through Symphonia, chosen by extension like `decode_audio_any`.
    pub fn open(path: &str) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("open: {e}"))?;
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        let bytes_read = Arc::new(AtomicU64::new(0));
        let counting = CountingFile {
            file,
            pos: bytes_read.clone(),
            len,
        };
        let is_wav = path.to_ascii_lowercase().ends_with(".wav");
        let (source, sample_rate, channels, total_frames) = if is_wav {
            let reader = hound::WavReader::new(BufReader::new(counting))
                .map_err(|e| format!("open: {e}"))?;
            let spec = reader.spec();
            if spec.sample_rate == 0 || spec.channels == 0 {
                return Err("invalid WAV header (sample_rate or channels = 0)".into());
            }
            if spec.sample_format == hound::SampleFormat::Int && spec.bits_per_sample == 0 {
                return Err("invalid bits_per_sample 0".into());
            }
            let frames = reader.duration() as u64;
            (
                Source::Wav(reader),
                spec.sample_rate,
                spec.channels as usize,
                Some(frames),
            )
        } else {
            let (src, rate, ch, frames) = open_symphonia(path, counting)?;
            (Source::Symphonia(Box::new(src)), rate, ch, frames)
        };
        assert!(
            sample_rate > 0 && channels > 0,
            "stream must have a rate and channels"
        );
        Ok(Self {
            source,
            sample_rate,
            total_frames,
            file_len: len,
            bytes_read,
            channels,
            frame_acc: 0.0,
            frame_fill: 0,
            frames_out: 0,
            done: false,
        })
    }

    /// Append the next decoded mono samples to `out`. Returns false once the
    /// stream is exhausted (nothing was appended). Decode errors are handled
    /// exactly as the whole-file decoders did: skipped where they were
    /// skipped, end of stream where they ended it.
    pub fn read_block(&mut self, out: &mut Vec<f32>) -> bool {
        if self.done {
            return false;
        }
        let before = out.len();
        // Decode until at least one mono sample came out or the stream ends.
        while out.len() == before && !self.done {
            match &mut self.source {
                Source::Wav(reader) => {
                    let spec = reader.spec();
                    let mut got = 0usize;
                    match spec.sample_format {
                        hound::SampleFormat::Int => {
                            let scale = (1i64 << (spec.bits_per_sample as i32 - 1)) as f32;
                            for s in reader.samples::<i32>().take(WAV_BLOCK_SAMPLES) {
                                got += 1;
                                if let Ok(v) = s {
                                    push_sample(
                                        v as f32 / scale,
                                        self.channels,
                                        &mut self.frame_acc,
                                        &mut self.frame_fill,
                                        out,
                                    );
                                }
                            }
                        }
                        hound::SampleFormat::Float => {
                            for s in reader.samples::<f32>().take(WAV_BLOCK_SAMPLES) {
                                got += 1;
                                if let Ok(v) = s {
                                    push_sample(
                                        v,
                                        self.channels,
                                        &mut self.frame_acc,
                                        &mut self.frame_fill,
                                        out,
                                    );
                                }
                            }
                        }
                    }
                    if got == 0 {
                        self.done = true;
                    }
                }
                Source::Symphonia(src) => {
                    if !src.next_samples(
                        &mut self.channels,
                        &mut self.frame_acc,
                        &mut self.frame_fill,
                        out,
                    ) {
                        self.done = true;
                    }
                }
            }
        }
        self.frames_out += (out.len() - before) as u64;
        out.len() > before
    }

    /// Mono samples handed out so far.
    pub fn samples_read(&self) -> u64 {
        self.frames_out
    }

    /// Fraction of the file consumed, from the declared length when there is
    /// one, otherwise from the bytes read.
    pub fn progress(&self) -> f64 {
        let f = match self.total_frames {
            Some(t) if t > 0 => self.frames_out as f64 / t as f64,
            _ if self.file_len > 0 => {
                self.bytes_read.load(Ordering::Relaxed) as f64 / self.file_len as f64
            }
            _ => 0.0,
        };
        f.clamp(0.0, 1.0)
    }
}

/// Downmix one interleaved sample into the frame being assembled: the same
/// left-to-right sum / channels the whole-file decoders used, so the result
/// is bit-identical. A trailing partial frame is dropped, like
/// `chunks_exact` did.
#[inline]
fn push_sample(v: f32, channels: usize, acc: &mut f32, fill: &mut usize, out: &mut Vec<f32>) {
    *acc += v;
    *fill += 1;
    if *fill == channels {
        out.push(if channels == 1 {
            *acc
        } else {
            *acc / channels as f32
        });
        *acc = 0.0;
        *fill = 0;
    }
}

fn open_symphonia(
    path: &str,
    file: CountingFile,
) -> Result<(SymphoniaSource, u32, usize, Option<u64>), String> {
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = std::path::Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
    {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions {
                enable_gapless: true,
                ..Default::default()
            },
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("probe: {e}"))?;
    let format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| "no decodable audio track".to_string())?;
    let track_id = track.id;
    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| "missing sample rate in codec params".to_string())?;
    let channels = track
        .codec_params
        .channels
        .map(|c| c.count())
        .unwrap_or(1)
        .max(1);
    let total_frames = track.codec_params.n_frames;
    let decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("decoder: {e}"))?;
    Ok((
        SymphoniaSource {
            format,
            decoder,
            track_id,
            sample_buf: None,
            consecutive_packet_errs: 0,
        },
        sample_rate,
        channels,
        total_frames,
    ))
}

impl SymphoniaSource {
    /// Decode the next packet of our track into `out`. False at end of
    /// stream. Error handling is `ffi::decode_via_symphonia`'s, unchanged.
    ///
    /// `channels` starts as the header's count and is replaced by the
    /// decoded buffer's: an AAC m4a usually declares none, and the default
    /// of 1 read stereo as twice-as-long mono.
    fn next_samples(
        &mut self,
        channels: &mut usize,
        acc: &mut f32,
        fill: &mut usize,
        out: &mut Vec<f32>,
    ) -> bool {
        use symphonia::core::audio::SampleBuffer;
        use symphonia::core::errors::Error as SymphoniaError;
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => {
                    self.consecutive_packet_errs = 0;
                    p
                }
                // End-of-stream surfaces as IoError(UnexpectedEof).
                Err(SymphoniaError::IoError(_)) => return false,
                Err(SymphoniaError::ResetRequired) => return false,
                // Permissive: a malformed tail keeps what was decoded.
                Err(SymphoniaError::DecodeError(_)) => {
                    self.consecutive_packet_errs += 1;
                    if self.consecutive_packet_errs > 64 {
                        return false;
                    }
                    continue;
                }
                Err(_) => return false,
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(audio_buf) => {
                    if self.sample_buf.is_none() {
                        let spec = *audio_buf.spec();
                        *channels = spec.channels.count().max(1);
                        let duration = audio_buf.capacity() as u64;
                        self.sample_buf = Some(SampleBuffer::<f32>::new(duration, spec));
                    }
                    if let Some(buf) = &mut self.sample_buf {
                        buf.copy_interleaved_ref(audio_buf);
                        for &v in buf.samples() {
                            push_sample(v, *channels, acc, fill, out);
                        }
                    }
                    return true;
                }
                // Single-packet decode errors are recoverable: next packet.
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(SymphoniaError::ResetRequired) => return false,
                Err(SymphoniaError::IoError(_)) => return false,
                Err(_) => continue,
            }
        }
    }
}

/// Filtered audio cut into chunks as it streams in. Holds at most one chunk
/// plus one decoded block.
pub struct FileChunker {
    buf: Vec<f32>,
    /// File sample index of `buf[0]`.
    buf_start: usize,
    sample_rate: u32,
    max_chunk_samples: usize,
    /// Cut at the latest pause in the last quarter (file load), or at
    /// exactly `max_chunk_samples` (meeting retranscribe windows).
    at_pauses: bool,
    highpass: crate::preprocess::FileLoadHighpass,
}

/// One chunk and where it starts in the file.
pub struct FileChunk {
    pub audio: crate::audio::ProcessedAudio,
    pub start_sample: usize,
}

impl FileChunker {
    pub fn new(sample_rate: u32, max_chunk_samples: usize, at_pauses: bool) -> Self {
        assert!(
            max_chunk_samples > 0,
            "FileChunker: max_chunk_samples must be positive"
        );
        Self {
            buf: Vec::with_capacity(max_chunk_samples + 16_384),
            buf_start: 0,
            sample_rate,
            max_chunk_samples,
            at_pauses,
            highpass: crate::preprocess::FileLoadHighpass::new(sample_rate),
        }
    }

    /// Filter `block` and append it.
    pub fn push(&mut self, block: &mut [f32]) {
        self.highpass.process(block);
        self.buf.extend_from_slice(block);
    }

    /// The next chunk once more than one chunk is buffered, so its end is
    /// final. At end of stream pass `eof = true` to drain the rest.
    pub fn next_chunk(&mut self, eof: bool) -> Option<FileChunk> {
        let end = if self.at_pauses && self.buf.len() > self.max_chunk_samples {
            crate::audio::first_silence_split(&self.buf, self.sample_rate, self.max_chunk_samples)
        } else if !self.at_pauses && self.buf.len() >= self.max_chunk_samples {
            self.max_chunk_samples
        } else if eof && !self.buf.is_empty() {
            self.buf.len()
        } else {
            return None;
        };
        let samples: Vec<f32> = self.buf.drain(..end).collect();
        let start_sample = self.buf_start;
        self.buf_start += end;
        Some(FileChunk {
            audio: crate::audio::ProcessedAudio {
                samples,
                sample_rate: self.sample_rate,
            },
            start_sample,
        })
    }
}

/// Stream `stream` through the file-load filter and hand each chunk of at
/// most `max_chunk_samples`, cut at pauses like `split_at_silence`, to
/// `on_chunk`, in order. `on_chunk` returns false to stop early. Returns the
/// number of mono samples decoded.
pub fn for_each_chunk<F>(stream: &mut FileAudioStream, max_chunk_samples: usize, on_chunk: F) -> u64
where
    F: FnMut(FileChunk, &FileAudioStream) -> bool,
{
    let chunker = FileChunker::new(stream.sample_rate, max_chunk_samples, true);
    run(stream, chunker, on_chunk)
}

/// The same with fixed `window_samples` windows (the last one shorter).
pub fn for_each_window<F>(stream: &mut FileAudioStream, window_samples: usize, on_chunk: F) -> u64
where
    F: FnMut(FileChunk, &FileAudioStream) -> bool,
{
    let chunker = FileChunker::new(stream.sample_rate, window_samples, false);
    run(stream, chunker, on_chunk)
}

fn run<F>(stream: &mut FileAudioStream, mut chunker: FileChunker, mut on_chunk: F) -> u64
where
    F: FnMut(FileChunk, &FileAudioStream) -> bool,
{
    let mut block: Vec<f32> = Vec::with_capacity(16_384);
    loop {
        block.clear();
        let more = stream.read_block(&mut block);
        if more {
            chunker.push(&mut block);
        }
        while let Some(chunk) = chunker.next_chunk(!more) {
            if !on_chunk(chunk, stream) {
                return stream.samples_read();
            }
        }
        if !more {
            return stream.samples_read();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav(path: &std::path::Path, sr: u32, channels: u16, frames: &[Vec<f32>]) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: sr,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for f in frames {
            for &s in f {
                w.write_sample((s * 32767.0) as i16).unwrap();
            }
        }
        w.finalize().unwrap();
    }

    /// Speech-like stereo with pauses and steady stretches, deterministic.
    fn test_frames(sr: u32, secs: f32) -> Vec<Vec<f32>> {
        let n = (sr as f32 * secs) as usize;
        let mut seed: u32 = 0x1234_5678;
        (0..n)
            .map(|i| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let noise = (seed as f32 / u32::MAX as f32) * 2.0 - 1.0;
                let t = i as f32 / sr as f32;
                let amp = if (t % 9.7) < 0.5 { 0.001 } else { 0.25 };
                let l = (t * 220.0 * std::f32::consts::TAU).sin() * amp + noise * amp * 0.3;
                vec![l, l * 0.7 - noise * 0.01]
            })
            .collect()
    }

    #[test]
    fn streamed_chunks_match_the_whole_file_path() {
        let dir = std::env::temp_dir().join(format!("dimmy-fstream-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stereo.wav");
        let sr = 16_000;
        write_wav(&path, sr, 2, &test_frames(sr, 140.0));
        let p = path.to_string_lossy().to_string();

        // Whole-file path, as dimmy_transcribe_file did it.
        let (mono, rate) = crate::ffi::decode_audio_any(&p).unwrap();
        let processed = crate::preprocess::process_buffer_for_file_load(&mono, rate);
        let max = 30 * rate as usize;
        let expected = crate::audio::ProcessedAudio {
            samples: processed.clone(),
            sample_rate: rate,
        }
        .split_at_silence(max);

        let mut stream = FileAudioStream::open(&p).unwrap();
        assert_eq!(stream.sample_rate, rate);
        let mut got: Vec<FileChunk> = Vec::new();
        let total = for_each_chunk(&mut stream, max, |c, _| {
            got.push(c);
            true
        });

        assert_eq!(total as usize, mono.len(), "every sample decoded");
        assert_eq!(got.len(), expected.len(), "same number of chunks");
        let mut start = 0;
        for (i, (g, e)) in got.iter().zip(expected.iter()).enumerate() {
            assert_eq!(g.start_sample, start, "chunk {i} start");
            assert_eq!(g.audio.samples, e.samples, "chunk {i} samples differ");
            start += e.samples.len();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A stereo AAC m4a declares no channel count in its codec parameters.
    /// Reading the count from there (default 1) treated L,R,L,R as mono
    /// samples: twice the length, half speed, an octave down. Every stereo
    /// m4a loaded before 2026-10-22 was transcribed that way.
    #[test]
    fn stereo_m4a_decodes_at_its_real_length() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/stereo_2s.m4a");

        let (mono, rate) = crate::ffi::decode_via_symphonia(path).unwrap();
        let whole_secs = mono.len() as f64 / rate as f64;
        assert!(
            (whole_secs - 2.0).abs() < 0.1,
            "whole-file decode: {whole_secs:.3}s, expected ~2s"
        );

        let mut stream = FileAudioStream::open(path).unwrap();
        let mut n = 0u64;
        let mut block = Vec::new();
        while stream.read_block(&mut block) {
            n += block.len() as u64;
            block.clear();
        }
        let streamed_secs = n as f64 / stream.sample_rate as f64;
        assert!(
            (streamed_secs - 2.0).abs() < 0.1,
            "streamed decode: {streamed_secs:.3}s, expected ~2s"
        );
        assert_eq!(n as usize, mono.len(), "both decoders agree");
    }

    #[test]
    fn stopping_early_reads_no_further() {
        let dir = std::env::temp_dir().join(format!("dimmy-fstream-stop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mono.wav");
        let sr = 16_000;
        let frames: Vec<Vec<f32>> = test_frames(sr, 100.0)
            .into_iter()
            .map(|f| vec![f[0]])
            .collect();
        write_wav(&path, sr, 1, &frames);
        let mut stream = FileAudioStream::open(&path.to_string_lossy()).unwrap();
        let mut n = 0;
        let read = for_each_chunk(&mut stream, 10 * sr as usize, |_, _| {
            n += 1;
            n < 2
        });
        assert_eq!(n, 2);
        assert!(
            read < frames.len() as u64,
            "stopped before the end ({read} read)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn progress_reaches_one_at_the_end() {
        let dir = std::env::temp_dir().join(format!("dimmy-fstream-prog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.wav");
        let sr = 8_000;
        let frames: Vec<Vec<f32>> = test_frames(sr, 50.0)
            .into_iter()
            .map(|f| vec![f[0]])
            .collect();
        write_wav(&path, sr, 1, &frames);
        let mut stream = FileAudioStream::open(&path.to_string_lossy()).unwrap();
        let mut last = 0.0;
        for_each_chunk(&mut stream, 10 * sr as usize, |_, s| {
            assert!(s.progress() >= last, "progress went backwards");
            last = s.progress();
            true
        });
        assert!(
            (stream.progress() - 1.0).abs() < 1e-9,
            "progress {}",
            stream.progress()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
