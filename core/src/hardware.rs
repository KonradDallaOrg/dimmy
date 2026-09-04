//! What the machine can do — for the one decision the user has to make
//! blind at onboarding: local transcription, or cloud.
//!
//! The onboarding "Local" card promises "private · offline · runs on your
//! machine" and says nothing about whether the machine can. Meanwhile the
//! model catalog talks in requirements ("fits 4GB VRAM", "needs 6GB+"),
//! which asks the user to compare against a number they do not have. This
//! module supplies that number.
//!
//! # VRAM is a floor, not a forecast
//!
//! Measured 2026-09-04 on the same 4 GB NVIDIA T600, same model, same
//! recording: ~8 s per 15-second window in the morning, ~2 s in the
//! afternoon. Nothing about the VRAM changed — the GPU's power limit was
//! stuck at 20 W of 35. A spec-based verdict would have said "your GPU is
//! fine" with total confidence while a third of the windows were being
//! dropped.
//!
//! So `assess` answers only what specs can actually answer: will the model
//! FIT, or will it spill to the CPU. It never predicts speed, and
//! `Unknown` is a real answer — when detection fails we say so rather than
//! guessing, because a wrong confident recommendation is worse than none.

/// What we could learn about the graphics device. Every field is
/// `Option`: detection is best-effort on all three platforms and a
//. missing value must never be filled in with a plausible-looking guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuInfo {
    /// Adapter / chip name as the driver reports it.
    pub name: Option<String>,
    /// Memory the model can actually live in, in MiB. For a discrete card
    /// this is its dedicated VRAM; for an integrated or unified device it
    /// is the system memory it shares.
    pub vram_mb: Option<u64>,
    /// True for a discrete card with its own memory. False for integrated
    /// graphics and for Apple's unified memory — which are very different
    /// from each other, hence `apple_silicon` below.
    pub dedicated: bool,
    /// Apple silicon: unified memory AND the Neural Engine, so the usual
    /// "integrated graphics are hopeless" reasoning does not apply.
    pub apple_silicon: bool,
}

/// Whether local transcription is a reasonable default on this machine.
/// Deliberately coarse — this drives one line of onboarding copy and a
/// default, not a gate. The user can always choose Local anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalFitness {
    /// Models in the catalog fit. Local is a good default.
    Good,
    /// The smaller models fit, the larger ones will spill to CPU.
    Tight,
    /// Nothing in the catalog fits comfortably; cloud is the better start.
    Poor,
    /// We could not tell. Say so.
    Unknown,
}

/// whisper large-v3-turbo q8_0 is ~1.6 GB of weights before activations
/// and KV, and the local-LLM catalog's own descriptions start at "fits 4GB
/// VRAM" — so the 4 GB card class is the line above which the shipped
/// defaults fit.
///
/// The constant is 3800, not 4096, and the difference is not a fudge: a
/// card reserves part of its memory, so DXGI reports 3938 MB for the 4 GB
/// T600 this was first run on. At 4096 every nominal-4 GB card in
/// existence would fall into `Tight` and be told "smaller models only" —
/// advice this very machine disproves, having run large-v3-turbo at ~2 s
/// per 15-second window the same afternoon. The threshold is about the
/// card class, and the reported figure always sits just under it.
const GOOD_VRAM_MB: u64 = 3800;
/// Below 2 GB not even the turbo STT model fits without spilling, so the
/// smaller-model advice stops being true.
const TIGHT_VRAM_MB: u64 = 2048;

/// The whole decision, as a pure function of what detection found.
///
/// Integrated graphics on Windows/Linux are `Poor` regardless of how much
/// system memory they can address: the memory is shared, its bandwidth is
/// a fraction of a discrete card's, and there is little compute behind it.
/// Apple silicon shares memory too and is `Good` anyway — unified memory
/// has real bandwidth, and Parakeet runs on the Neural Engine at 100-300x
/// realtime.
pub fn assess(info: &GpuInfo) -> LocalFitness {
    if info.apple_silicon {
        return LocalFitness::Good;
    }
    let Some(vram) = info.vram_mb else {
        return LocalFitness::Unknown;
    };
    assert!(vram > 0, "detection must report None, not zero VRAM");
    if !info.dedicated {
        return LocalFitness::Poor;
    }
    if vram >= GOOD_VRAM_MB {
        LocalFitness::Good
    } else if vram >= TIGHT_VRAM_MB {
        LocalFitness::Tight
    } else {
        LocalFitness::Poor
    }
}

/// One line for the user, in their own terms. Returns `None` when there is
/// nothing honest to say, so the host renders no line at all rather than a
/// placeholder.
pub fn summary_line(info: &GpuInfo) -> Option<String> {
    let fitness = assess(info);
    let device = match (&info.name, info.vram_mb) {
        (Some(n), Some(mb)) if info.dedicated => format!("{} · {}", n, human_mb(mb)),
        (Some(n), Some(mb)) => format!("{} · {} shared", n, human_mb(mb)),
        (Some(n), None) => n.clone(),
        (None, Some(mb)) => human_mb(mb),
        (None, None) => return None,
    };
    let verdict = match fitness {
        LocalFitness::Good => "transcription runs here",
        LocalFitness::Tight => "smaller models only",
        LocalFitness::Poor => "Cloud is a better start",
        LocalFitness::Unknown => return Some(device),
    };
    Some(format!("{} — {}", device, verdict))
}

fn human_mb(mb: u64) -> String {
    assert!(mb > 0, "human_mb needs a real size");
    if mb >= 1024 {
        let gb = mb as f64 / 1024.0;
        if (gb - gb.round()).abs() < 0.05 {
            format!("{:.0} GB", gb)
        } else {
            format!("{:.1} GB", gb)
        }
    } else {
        format!("{} MB", mb)
    }
}

/// Best-effort hardware probe. Cheap enough to call from onboarding and
/// from the diagnostics page; not cached, because a user who plugs in an
/// eGPU or updates a driver should see the new answer without a restart.
pub fn detect() -> GpuInfo {
    #[cfg(target_os = "windows")]
    {
        detect_windows()
    }
    #[cfg(target_os = "macos")]
    {
        detect_macos()
    }
    #[cfg(target_os = "linux")]
    {
        detect_linux()
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        GpuInfo {
            name: None,
            vram_mb: None,
            dedicated: false,
            apple_silicon: false,
        }
    }
}

#[cfg(target_os = "windows")]
fn detect_windows() -> GpuInfo {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG, DXGI_ADAPTER_FLAG_SOFTWARE,
    };

    let mut best = GpuInfo {
        name: None,
        vram_mb: None,
        dedicated: false,
        apple_silicon: false,
    };
    unsafe {
        let factory: IDXGIFactory1 = match CreateDXGIFactory1() {
            Ok(f) => f,
            Err(e) => {
                crate::log(&format!("[Hardware] DXGI factory failed: {e}"));
                return best;
            }
        };
        // Pick the adapter with the most dedicated VRAM: a laptop enumerates
        // the integrated GPU first, and the discrete card is the one the
        // model will actually load into (see local_stt.rs discrete-GPU pick).
        let mut best_dedicated: u64 = 0;
        let mut best_shared: u64 = 0;
        for i in 0.. {
            let adapter = match factory.EnumAdapters1(i) {
                Ok(a) => a,
                Err(_) => break,
            };
            let Ok(desc) = adapter.GetDesc1() else {
                continue;
            };
            // The Microsoft Basic Render Driver would otherwise look like a
            // GPU with a lot of shared memory and no dedicated VRAM.
            if DXGI_ADAPTER_FLAG(desc.Flags as i32) == DXGI_ADAPTER_FLAG_SOFTWARE {
                continue;
            }
            let name = String::from_utf16_lossy(&desc.Description)
                .trim_end_matches('\0')
                .trim()
                .to_string();
            let dedicated = desc.DedicatedVideoMemory as u64 / (1024 * 1024);
            let shared = desc.SharedSystemMemory as u64 / (1024 * 1024);
            if dedicated > best_dedicated {
                best_dedicated = dedicated;
                best = GpuInfo {
                    name: Some(name),
                    vram_mb: Some(dedicated),
                    dedicated: true,
                    apple_silicon: false,
                };
            } else if best_dedicated == 0 && shared > best_shared {
                // No discrete card seen yet — remember the integrated one so
                // we can still name it instead of reporting nothing.
                best_shared = shared;
                best = GpuInfo {
                    name: Some(name),
                    vram_mb: Some(shared),
                    dedicated: false,
                    apple_silicon: false,
                };
            }
        }
    }
    best
}

#[cfg(target_os = "macos")]
fn detect_macos() -> GpuInfo {
    // Unified memory: the model lives in system RAM, which on Apple silicon
    // has the bandwidth to make that fine. Read the total rather than a
    // Metal working-set budget — the budget is a soft hint and needs the
    // Metal framework, and the honest number for "will it fit" is the RAM.
    let vram_mb = sysctl_u64("hw.memsize").map(|b| b / (1024 * 1024));
    let apple_silicon = cfg!(target_arch = "aarch64");
    let name = sysctl_string("machdep.cpu.brand_string").or_else(|| {
        if apple_silicon {
            Some("Apple Silicon".to_string())
        } else {
            None
        }
    });
    GpuInfo {
        name,
        vram_mb,
        dedicated: false,
        apple_silicon,
    }
}

#[cfg(target_os = "macos")]
fn sysctl_u64(key: &str) -> Option<u64> {
    let out = std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", key])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

#[cfg(target_os = "macos")]
fn sysctl_string(key: &str) -> Option<String> {
    let out = std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", key])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(target_os = "linux")]
fn detect_linux() -> GpuInfo {
    // AMD and Intel expose VRAM through sysfs; NVIDIA does not, and shelling
    // out to nvidia-smi at onboarding is not worth it. An unknown answer is
    // an acceptable one here — Linux has no meeting mode and the local-model
    // path is the least-travelled of the three platforms.
    let mut info = GpuInfo {
        name: None,
        vram_mb: None,
        dedicated: false,
        apple_silicon: false,
    };
    let Ok(cards) = std::fs::read_dir("/sys/class/drm") else {
        return info;
    };
    for entry in cards.flatten() {
        let path = entry.path();
        let Some(stem) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !stem.starts_with("card") || stem.contains('-') {
            continue;
        }
        let vram = std::fs::read_to_string(path.join("device/mem_info_vram_total"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(|b| b / (1024 * 1024));
        if let Some(mb) = vram.filter(|mb| *mb > 0) {
            if info.vram_mb.is_none_or(|prev| mb > prev) {
                info.vram_mb = Some(mb);
                info.dedicated = true;
                info.name = std::fs::read_to_string(path.join("device/uevent"))
                    .ok()
                    .and_then(|s| {
                        s.lines()
                            .find_map(|l| l.strip_prefix("DRIVER=").map(|d| d.to_string()))
                    })
                    .or(info.name.take());
            }
        }
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(vram_mb: Option<u64>, dedicated: bool) -> GpuInfo {
        GpuInfo {
            name: Some("Test GPU".into()),
            vram_mb,
            dedicated,
            apple_silicon: false,
        }
    }

    #[test]
    fn a_discrete_card_with_room_is_good() {
        assert_eq!(assess(&gpu(Some(8192), true)), LocalFitness::Good);
        assert_eq!(assess(&gpu(Some(4096), true)), LocalFitness::Good);
    }

    #[test]
    fn a_nominal_four_gb_card_counts_as_four_gb() {
        // Observed on the machine this was written on: a 4 GB T600 reports
        // 3938 MB because the card reserves a slice. A threshold of 4096
        // would tell every 4 GB card "smaller models only" — which that
        // same machine disproved by running large-v3-turbo at ~2 s per
        // window. Regression guard for the real number.
        assert_eq!(assess(&gpu(Some(3938), true)), LocalFitness::Good);
        assert_eq!(assess(&gpu(Some(4096), true)), LocalFitness::Good);
    }

    #[test]
    fn below_the_card_class_is_tight() {
        assert_eq!(assess(&gpu(Some(3799), true)), LocalFitness::Tight);
    }

    #[test]
    fn a_small_card_takes_smaller_models() {
        assert_eq!(assess(&gpu(Some(2048), true)), LocalFitness::Tight);
        assert_eq!(assess(&gpu(Some(2047), true)), LocalFitness::Poor);
    }

    #[test]
    fn integrated_graphics_are_poor_however_much_they_address() {
        // 32 GB of addressable shared memory does not make an Intel iGPU a
        // place to run whisper: the bandwidth and the compute are not there.
        assert_eq!(assess(&gpu(Some(32768), false)), LocalFitness::Poor);
    }

    #[test]
    fn apple_silicon_is_good_despite_sharing_memory() {
        let mac = GpuInfo {
            name: Some("Apple M2 Pro".into()),
            vram_mb: Some(16384),
            dedicated: false,
            apple_silicon: true,
        };
        assert_eq!(assess(&mac), LocalFitness::Good);
    }

    #[test]
    fn apple_silicon_is_good_even_before_we_know_the_memory() {
        // Metal is always present and the Neural Engine does not depend on a
        // memory figure we may have failed to read.
        let mac = GpuInfo {
            name: None,
            vram_mb: None,
            dedicated: false,
            apple_silicon: true,
        };
        assert_eq!(assess(&mac), LocalFitness::Good);
    }

    #[test]
    fn a_failed_probe_says_unknown_rather_than_guessing() {
        assert_eq!(assess(&gpu(None, false)), LocalFitness::Unknown);
        assert_eq!(assess(&gpu(None, true)), LocalFitness::Unknown);
    }

    #[test]
    fn nothing_detected_produces_no_line_at_all() {
        let blank = GpuInfo {
            name: None,
            vram_mb: None,
            dedicated: false,
            apple_silicon: false,
        };
        assert_eq!(summary_line(&blank), None);
    }

    #[test]
    fn the_line_names_the_device_and_the_advice() {
        let line = summary_line(&gpu(Some(4096), true)).expect("line");
        assert_eq!(line, "Test GPU · 4 GB — transcription runs here");
    }

    #[test]
    fn shared_memory_is_labelled_as_shared() {
        let line = summary_line(&gpu(Some(8192), false)).expect("line");
        assert_eq!(line, "Test GPU · 8 GB shared — Cloud is a better start");
    }

    #[test]
    fn an_unknown_probe_states_the_device_without_advice() {
        // Naming the GPU while admitting we cannot judge it is honest; the
        // alternative is inventing a recommendation.
        let info = GpuInfo {
            name: Some("Some GPU".into()),
            vram_mb: None,
            dedicated: true,
            apple_silicon: false,
        };
        assert_eq!(summary_line(&info).as_deref(), Some("Some GPU"));
    }

    #[test]
    fn odd_sizes_keep_one_decimal() {
        assert_eq!(human_mb(6144), "6 GB");
        assert_eq!(human_mb(3072), "3 GB");
        assert_eq!(human_mb(1536), "1.5 GB");
        assert_eq!(human_mb(512), "512 MB");
    }

    #[test]
    fn detect_never_panics_on_this_machine() {
        // The probe runs on every platform in CI; a driver that answers
        // strangely must produce a None, not a crash.
        let info = detect();
        // Printed so a CI log (and a support ticket) records what this
        // machine actually answered, not just that it did not crash.
        eprintln!(
            "[hardware] {:?} -> {:?} | {:?}",
            info,
            assess(&info),
            summary_line(&info)
        );
        if let Some(mb) = info.vram_mb {
            assert!(mb > 0, "zero must be reported as None");
        }
        let _ = assess(&info);
        let _ = summary_line(&info);
    }
}
