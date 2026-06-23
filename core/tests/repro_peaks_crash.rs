// Throwaway repro for the meeting-open crash (fastfail 7 in dimmy_lib while
// selecting a 60-min meeting). Calls the same FFI the host uses to render the
// waveform on the offending audio.ogg and prints the outcome. Run with:
//   DIMMY_REPRO_OGG="C:\\...\\audio.ogg" cargo test --release --test repro_peaks_crash --features local-stt -- --nocapture
use std::ffi::CString;

#[test]
fn compute_peaks_on_offending_ogg() {
    let path = match std::env::var("DIMMY_REPRO_OGG") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("DIMMY_REPRO_OGG not set — skipping");
            return;
        }
    };
    eprintln!("repro: computing peaks for {path}");
    let c = CString::new(path).unwrap();
    // Mirror the host: 400 buckets, host-sized buffer (400*10+256 → 4096).
    let mut buf = vec![0i8; 8 * 1024];
    let rc = unsafe {
        dimmy_lib::ffi::dimmy_compute_audio_peaks(
            c.as_ptr(),
            400,
            buf.as_mut_ptr(),
            buf.len() as i32,
        )
    };
    eprintln!("repro: dimmy_compute_audio_peaks rc={rc}");
    assert!(rc > 0, "expected success, got rc={rc}");
    let json = unsafe {
        std::ffi::CStr::from_ptr(buf.as_ptr())
            .to_string_lossy()
            .into_owned()
    };
    let dur = json
        .split("\"duration_secs\":")
        .nth(1)
        .and_then(|s| s.split(',').next())
        .unwrap_or("?");
    let n = json.matches(',').count(); // rough peak count
    eprintln!("repro: duration_secs={dur} approx_peaks={n}");
}
