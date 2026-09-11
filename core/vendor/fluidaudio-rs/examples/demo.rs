//! Demo: Test FluidAudio Rust bindings
//!
//! Usage: cargo run --example demo [audio_file]

use fluidaudio_rs::FluidAudio;
use std::env;

fn main() {
    println!("FluidAudio Rust Demo");
    println!("====================\n");

    // Create instance
    print!("Creating FluidAudio instance... ");
    let audio = match FluidAudio::new() {
        Ok(a) => {
            println!("OK");
            a
        }
        Err(e) => {
            println!("FAILED: {}", e);
            return;
        }
    };

    // Get system info
    println!("\n--- System Info ---");
    let info = audio.system_info();
    println!("Platform: {}", info.platform);
    println!("Chip: {}", info.chip_name);
    println!("Memory: {:.1} GB", info.memory_gb);
    println!("Apple Silicon: {}", info.is_apple_silicon);

    // Check for audio file argument
    let args: Vec<String> = env::args().collect();
    if let Some(audio_path) = args.get(1) {
        println!("\n--- ASR Test ---");
        println!("Audio file: {}", audio_path);

        print!("Initializing ASR (may take 20-30s on first run)... ");
        match audio.init_asr() {
            Ok(_) => println!("OK"),
            Err(e) => {
                println!("FAILED: {}", e);
                return;
            }
        }

        print!("Transcribing... ");
        match audio.transcribe_file(audio_path) {
            Ok(result) => {
                println!("OK\n");
                println!("Text: {}", result.text);
                println!("Confidence: {:.1}%", result.confidence * 100.0);
                println!("Duration: {:.2}s", result.duration);
                println!("Processing: {:.2}s", result.processing_time);
                println!("Speed: {:.1}x realtime", result.rtfx);
            }
            Err(e) => {
                println!("FAILED: {}", e);
            }
        }
    } else {
        println!("\n--- ASR Test ---");
        println!("Skipped (no audio file provided)");
        println!("To test ASR: cargo run --example demo -- path/to/audio.wav");
    }

    println!("\nDemo complete!");
}
