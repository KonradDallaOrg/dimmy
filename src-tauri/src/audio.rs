use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::io::Cursor;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

/// Commands sent to the audio capture thread
pub enum AudioCommand {
    Start(Option<String>), // Optional device name; None = system default
    Stop,
}

/// List available input device names
pub fn list_input_devices() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|devs| devs.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

/// Spawn a dedicated audio capture thread.
/// Returns a sender to control the thread.
/// Captured samples are written to the shared buffer.
pub fn spawn_audio_thread(buffer: Arc<Mutex<Vec<f32>>>) -> mpsc::Sender<AudioCommand> {
    let (tx, rx) = mpsc::channel::<AudioCommand>();

    thread::spawn(
        #[allow(unused_assignments, unused_variables)]
        move || {
            let host = cpal::default_host();
            // stream is held alive to keep recording; dropping/replacing stops/starts
            let mut stream: Option<cpal::Stream> = None;

            // Event loop: wait for commands
            for cmd in rx {
                match cmd {
                    AudioCommand::Start(device_name) => {
                        // Find the requested device, or fall back to default
                        let device = if let Some(ref name) = device_name {
                            host.input_devices()
                                .ok()
                                .and_then(|mut devs| {
                                    devs.find(|d| d.name().ok().as_deref() == Some(name.as_str()))
                                })
                                .or_else(|| host.default_input_device())
                        } else {
                            host.default_input_device()
                        };

                        let device = match device {
                            Some(d) => d,
                            None => {
                                eprintln!("No input device available");
                                continue;
                            }
                        };

                        let config = match device.default_input_config() {
                            Ok(c) => c,
                            Err(e) => {
                                eprintln!("Failed to get input config: {}", e);
                                continue;
                            }
                        };

                        let channels = config.channels() as usize;

                        // Clear buffer
                        if let Ok(mut b) = buffer.lock() {
                            b.clear();
                        }

                        let buf = buffer.clone();
                        let s = match config.sample_format() {
                            cpal::SampleFormat::F32 => device.build_input_stream(
                                &config.clone().into(),
                                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                                    if let Ok(mut b) = buf.lock() {
                                        if channels > 1 {
                                            for chunk in data.chunks(channels) {
                                                let mono =
                                                    chunk.iter().sum::<f32>() / channels as f32;
                                                b.push(mono);
                                            }
                                        } else {
                                            b.extend_from_slice(data);
                                        }
                                    }
                                },
                                |err| eprintln!("Audio error: {}", err),
                                None,
                            ),
                            cpal::SampleFormat::I16 => {
                                let buf2 = buffer.clone();
                                device.build_input_stream(
                                    &config.clone().into(),
                                    move |data: &[i16], _: &cpal::InputCallbackInfo| {
                                        if let Ok(mut b) = buf2.lock() {
                                            for chunk in data.chunks(channels) {
                                                let mono: f32 = chunk
                                                    .iter()
                                                    .map(|&s| s as f32 / 32768.0)
                                                    .sum::<f32>()
                                                    / channels as f32;
                                                b.push(mono);
                                            }
                                        }
                                    },
                                    |err| eprintln!("Audio error: {}", err),
                                    None,
                                )
                            }
                            _ => {
                                eprintln!("Unsupported sample format");
                                continue;
                            }
                        };

                        match s {
                            Ok(s) => {
                                let _ = s.play();
                                stream = Some(s);
                            }
                            Err(e) => eprintln!("Failed to build stream: {}", e),
                        }
                    }
                    AudioCommand::Stop => {
                        // Dropping the stream stops recording
                        stream = None;
                    }
                }
            }
        },
    );

    tx
}

/// Get the default input device name
pub fn default_input_device_name() -> String {
    let host = cpal::default_host();
    host.default_input_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_else(|| "No device".to_string())
}

/// Get sample rate for a specific device (by name), falling back to default
pub fn device_sample_rate(device_name: &Option<String>) -> u32 {
    let host = cpal::default_host();
    let device = if let Some(ref name) = device_name {
        host.input_devices()
            .ok()
            .and_then(|mut devs| devs.find(|d| d.name().ok().as_deref() == Some(name.as_str())))
            .or_else(|| host.default_input_device())
    } else {
        host.default_input_device()
    };
    device
        .and_then(|d| d.default_input_config().ok())
        .map(|c| c.sample_rate().0)
        .unwrap_or(44100)
}

/// Encode f32 samples to WAV bytes (16-bit PCM, mono)
pub fn encode_wav(
    samples: &[f32],
    sample_rate: u32,
) -> Result<Vec<u8>, crate::error::AudioError> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec)?;
        for &sample in samples {
            let s16 = (sample * 32767.0).clamp(-32768.0, 32767.0) as i16;
            writer.write_sample(s16)?;
        }
        writer.finalize()?;
    }
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_wav_produces_valid_header() {
        let samples = vec![0.0f32; 100];
        let wav = encode_wav(&samples, 44100).expect("encode_wav failed");

        // RIFF header check
        assert_eq!(&wav[0..4], b"RIFF", "Missing RIFF header");
        assert_eq!(&wav[8..12], b"WAVE", "Missing WAVE identifier");

        // Read with hound to validate structure
        let reader = hound::WavReader::new(Cursor::new(&wav)).expect("hound can't read WAV");
        let spec = reader.spec();
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.sample_rate, 44100);
        assert_eq!(spec.bits_per_sample, 16);
        assert_eq!(spec.sample_format, hound::SampleFormat::Int);
    }

    #[test]
    fn encode_wav_correct_sample_count() {
        let samples = vec![0.5, -0.5, 0.0, 1.0, -1.0];
        let wav = encode_wav(&samples, 16000).expect("encode_wav failed");

        let reader = hound::WavReader::new(Cursor::new(&wav)).expect("hound can't read WAV");
        assert_eq!(reader.len() as usize, samples.len());
        assert_eq!(reader.spec().sample_rate, 16000);
    }

    #[test]
    fn encode_wav_clamps_out_of_range() {
        // Values beyond [-1.0, 1.0] should be clamped, not wrap
        let samples = vec![2.0, -2.0, 5.0];
        let wav = encode_wav(&samples, 44100).expect("encode_wav failed");

        let mut reader = hound::WavReader::new(Cursor::new(&wav)).expect("hound can't read WAV");
        let decoded: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();
        assert_eq!(decoded[0], 32767, "Positive overflow not clamped to max");
        assert_eq!(decoded[1], -32768, "Negative overflow not clamped to min");
        assert_eq!(decoded[2], 32767, "Large positive not clamped");
    }

    #[test]
    fn encode_wav_silence_is_zeros() {
        let samples = vec![0.0f32; 50];
        let wav = encode_wav(&samples, 48000).expect("encode_wav failed");

        let mut reader = hound::WavReader::new(Cursor::new(&wav)).expect("hound can't read WAV");
        let decoded: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();
        assert!(
            decoded.iter().all(|&s| s == 0),
            "Silent input must produce zero samples"
        );
    }

    #[test]
    fn encode_wav_empty_input() {
        let samples: Vec<f32> = vec![];
        let wav = encode_wav(&samples, 44100).expect("encode_wav failed on empty input");

        let reader = hound::WavReader::new(Cursor::new(&wav)).expect("hound can't read WAV");
        assert_eq!(reader.len(), 0);
    }

    #[test]
    fn encode_wav_preserves_polarity() {
        let samples = vec![0.5f32, -0.5f32];
        let wav = encode_wav(&samples, 44100).expect("encode_wav failed");

        let mut reader = hound::WavReader::new(Cursor::new(&wav)).expect("hound can't read WAV");
        let decoded: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();
        assert!(decoded[0] > 0, "Positive sample must produce positive i16");
        assert!(decoded[1] < 0, "Negative sample must produce negative i16");
    }

    #[test]
    fn device_sample_rate_returns_valid_rate() {
        // With None (default device), should return a reasonable rate
        let rate = device_sample_rate(&None);
        // Valid rates: 8000, 16000, 22050, 44100, 48000, 96000, etc.
        assert!(rate >= 8000, "Sample rate too low: {}", rate);
        assert!(rate <= 192000, "Sample rate too high: {}", rate);
    }

    #[test]
    fn device_sample_rate_unknown_device_falls_back() {
        // Non-existent device name should fall back to default, not panic
        let rate = device_sample_rate(&Some("NonExistentDevice12345".to_string()));
        assert!(rate >= 8000, "Fallback rate too low: {}", rate);
    }

    #[test]
    fn list_input_devices_does_not_panic() {
        // Should return a list (possibly empty on CI), never panic
        let devices = list_input_devices();
        // Just verify it returns without panic — count may be 0 in headless env
        assert!(devices.len() < 1000, "Unreasonable device count");
    }

    #[test]
    fn spawn_audio_thread_responds_to_stop() {
        let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
        let tx = spawn_audio_thread(buffer.clone());
        // Sending Stop should not panic even with no prior Start
        let _ = tx.send(AudioCommand::Stop);
        // Drop sender — thread should exit cleanly
        drop(tx);
    }
}
