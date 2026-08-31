//! Local speech-to-text via whisper.cpp (through the whisper-rs crate).
//!
//! Provides model discovery, downloading from HuggingFace, and local
//! transcription gated behind the `local-stt` Cargo feature.

use std::path::{Path, PathBuf};

use crate::error::TranscribeError;

// ── Model catalogue ───────────────────────────────────────────────

const MODEL_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";
pub const DEFAULT_MODEL: &str = "ggml-base-q8_0.bin";

pub struct LocalModel {
    pub name: &'static str,
    pub filename: &'static str,
    pub size_mb: u32,
    pub description: &'static str,
    /// Custom download URL. When `None`, uses `MODEL_BASE_URL/filename`.
    pub url: Option<&'static str>,
}

pub const AVAILABLE_MODELS: &[LocalModel] = &[
    // ── Standard Whisper (multilingual) ──────────────────────────
    LocalModel {
        name: "Tiny",
        filename: "ggml-tiny-q8_0.bin",
        size_mb: 42,
        description: "Fastest, lower accuracy",
        url: None,
    },
    LocalModel {
        name: "Base",
        filename: "ggml-base-q8_0.bin",
        size_mb: 78,
        description: "Good balance of speed and accuracy",
        url: None,
    },
    LocalModel {
        name: "Small",
        filename: "ggml-small-q5_1.bin",
        size_mb: 181,
        description: "High accuracy, slower",
        url: None,
    },
    LocalModel {
        name: "Medium",
        filename: "ggml-medium-q5_0.bin",
        size_mb: 514,
        description: "Very high accuracy, requires 2GB+ RAM",
        url: None,
    },
    // ── Large-v3-Turbo (multilingual, optimized) ─────────────────
    LocalModel {
        name: "Large-v3-Turbo Q5",
        filename: "ggml-large-v3-turbo-q5_0.bin",
        size_mb: 574,
        description: "Fast + high accuracy, all languages",
        url: None,
    },
    LocalModel {
        name: "Large-v3-Turbo Q8",
        filename: "ggml-large-v3-turbo-q8_0.bin",
        size_mb: 874,
        description: "Best turbo quality, all languages",
        url: None,
    },
    // ── Large-v3 (multilingual, max accuracy) ────────────────────
    LocalModel {
        name: "Large-v3 Q5",
        filename: "ggml-large-v3-q5_0.bin",
        size_mb: 1104,
        description: "Maximum accuracy, all languages, slow",
        url: None,
    },
    // ── Distil-Whisper (English only, fastest large-class) ───────
    LocalModel {
        name: "Distil-Large-v3.5 Q8 (EN)",
        filename: "ggml-distil-large-v3.5-q8_0.bin",
        size_mb: 818,
        description: "6x faster than Large-v3, English only",
        url: Some("https://huggingface.co/Pomni/distil-large-v3.5-ggml-allquants/resolve/main/ggml-distil-large-v3.5-q8_0.bin"),
    },
    LocalModel {
        name: "Distil-Large-v3.5 Q5 (EN)",
        filename: "ggml-distil-large-v3.5-q5_0.bin",
        size_mb: 538,
        description: "6x faster than Large-v3, English only, compact",
        url: Some("https://huggingface.co/Pomni/distil-large-v3.5-ggml-allquants/resolve/main/ggml-distil-large-v3.5-q5_0.bin"),
    },
];

// ── Model directory helpers ───────────────────────────────────────

/// Returns `<data_dir>/<config-namespace>/models` (e.g. `~/Library/Application Support/dimmy/models`).
///
/// The namespace segment honours `DIMMY_CONFIG_NAMESPACE` (compile-time env, set by
/// `staging-tester.yml` to `dimmy-staging`) so a side-by-side staging install reads
/// and writes its own model tree instead of clobbering the prod one. Burned 2026-05-17:
/// `staging-tester.yml`'s side-by-side install was reading whisper models from the
/// prod `dimmy/models/` dir, which only "worked" because the same machine had a prod
/// install — a clean staging-only machine would have failed silently.
pub fn model_directory() -> PathBuf {
    let base = dirs::data_dir().expect("data_dir must be available on all supported platforms");
    base.join(crate::config_dir_name()).join("models")
}

/// Check whether a given model file already exists on disk.
pub fn model_exists(filename: &str) -> bool {
    assert!(!filename.is_empty(), "model filename must not be empty");
    model_path(filename).is_file()
}

/// Full path to a model file inside the model directory.
pub fn model_path(filename: &str) -> PathBuf {
    assert!(!filename.is_empty(), "model filename must not be empty");
    model_directory().join(filename)
}

// ── Model download ────────────────────────────────────────────────

/// Download a model from HuggingFace to the local model directory.
///
/// - Skips the download if the model file already exists.
/// - Writes to a `.part` temp file and renames on completion (atomic).
/// - Calls `on_progress(bytes_downloaded, total_bytes)` during download.
///   `total_bytes` is `0` if the server didn't send `Content-Length`.
pub async fn download_model<F>(filename: &str, on_progress: F) -> Result<PathBuf, TranscribeError>
where
    F: Fn(u64, u64),
{
    assert!(!filename.is_empty(), "model filename must not be empty");
    assert!(
        filename.ends_with(".bin"),
        "model filename must end with .bin"
    );

    let dest = model_path(filename);
    if dest.is_file() {
        crate::log(&format!(
            "[LocalSTT] Model already exists: {}",
            dest.display()
        ));
        return Ok(dest);
    }

    let dir = model_directory();
    std::fs::create_dir_all(&dir).map_err(|e| {
        TranscribeError::LocalModel(format!(
            "failed to create model dir {}: {}",
            dir.display(),
            e
        ))
    })?;

    // Use per-model custom URL if available, otherwise default base URL.
    let url = AVAILABLE_MODELS
        .iter()
        .find(|m| m.filename == filename)
        .and_then(|m| m.url)
        .map(|u| u.to_string())
        .unwrap_or_else(|| format!("{}/{}", MODEL_BASE_URL, filename));
    crate::log(&format!("[LocalSTT] Downloading {} ...", url));

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| TranscribeError::LocalModel(format!("HTTP client error: {}", e)))?;

    // Resume + integrity (Range/If-Range + SHA-256 + ggml/GGUF magic) via the
    // shared download module — same path as the LLM + parakeet downloads.
    crate::download::download_resumable(&client, &url, &dest, &[b"lmgg", b"GGUF"], on_progress)
        .await
        .map_err(TranscribeError::LocalModel)?;

    crate::log(&format!("[LocalSTT] Download complete: {}", dest.display()));
    assert!(dest.is_file(), "model file must exist after download");

    Ok(dest)
}

// ── GPU backend availability + device selection ─────────────────
//
// Two questions to answer before loading a model:
//   1. Is the GPU backend usable at all (vulkan-1.dll loadable, vkCreateInstance
//      succeeds, ≥1 physical device enumerates)?
//   2. If so, which device index should whisper/llama use?
//
// On clean Windows machines without a Vulkan ICD (old Intel iGPU drivers,
// some VMs, stripped-down installs), the Vulkan loader may be present but
// return zero devices. Without an explicit fallback, ggml_vulkan hard-fails
// inside whisper_init / llama_init and crashes the whole process.
//
// `gpu_backend_status()` caches a single probe result for the life of the
// process. Callers branch on it: Available → GPU init with device index,
// Unavailable → force CPU backend via `use_gpu(false)` / `n_gpu_layers(0)`.
//
// On multi-GPU systems (Optimus: Intel iGPU + NVIDIA dGPU), device 0 is
// usually the integrated GPU. Intel iGPU Vulkan compute has historically
// been unstable for whisper — we prefer the first discrete GPU when present.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GpuBackendStatus {
    /// Vulkan (or Metal, on macOS) is usable; `device` is the index to pass to ggml.
    Available { device: std::ffi::c_int },
    /// GPU backend cannot be initialized. Callers MUST fall back to CPU inference.
    Unavailable,
}

/// Probe the GPU backend once per process (cached). See module notes above.
#[cfg(any(feature = "local-stt", feature = "local-llm"))]
pub(crate) fn gpu_backend_status() -> GpuBackendStatus {
    use std::sync::OnceLock;
    static CACHE: OnceLock<GpuBackendStatus> = OnceLock::new();
    *CACHE.get_or_init(compute_gpu_backend_status)
}

#[cfg(any(feature = "local-stt", feature = "local-llm"))]
fn compute_gpu_backend_status() -> GpuBackendStatus {
    // Install ggml log callbacks so whisper/llama's internal messages go
    // into dimmy.log — critical for diagnosing the exact cause of a GPU
    // abort on the NEXT run if this run crashes. Also log the environment
    // snapshot (vulkan-1.dll path, TdrDelay, registered ICDs, etc).
    crate::gpu_diag::install_ggml_log_callbacks();
    crate::gpu_diag::log_environment_snapshot();

    // Sticky known-bad check (cross-session): if a prior crash was recovered
    // and the driver fingerprint has not changed since, skip the GPU path
    // entirely instead of crashing once and recovering again every cold start.
    // Fingerprint mismatch → driver/ICD likely updated → clear the marker and
    // give GPU one more chance.
    if let Some(record) = crate::gpu_health::read_known_bad() {
        let current = crate::gpu_diag::compute_driver_fingerprint();
        if current == record.fingerprint {
            crate::log(&format!(
                "[GPU] Sticky known-bad marker present (since {}, context: {}). \
                 Driver fingerprint unchanged — keeping CPU backend. \
                 User can clear this from Settings > Debug > Retry GPU.",
                record.timestamp, record.context
            ));
            crate::gpu_diag::disable_vulkan_loader(
                "known-bad marker: prior GPU crash, driver fingerprint unchanged",
            );
            return GpuBackendStatus::Unavailable;
        }
        crate::log(&format!(
            "[GPU] Sticky known-bad marker present but driver fingerprint changed \
             (was: {}, now: {}). Clearing marker and retrying GPU.",
            record.fingerprint, current
        ));
        crate::gpu_health::clear_known_bad();
    }

    // Crash-recovery (session-scoped): if the previous process aborted during
    // GPU init, the short-lived sentinel file is still on disk. Force CPU for
    // this session so the user gets a working app instead of a crash loop,
    // and promote the recovery into a sticky known-bad record so the NEXT
    // cold start skips the crashing GPU path too. Clear the sentinel either
    // way so subsequent recoveries within this run don't loop on it.
    //
    // We also disable the Vulkan loader via env vars here. Setting
    // `use_gpu(false)` on whisper/llama params is NOT sufficient because
    // ggml_backend_registry unconditionally registers ggml-vulkan at
    // `WhisperContext::new_with_params` / `LlamaBackend::init()` time, and
    // `ggml_vk_instance_init` aborts the process on hosts where a device is
    // discoverable but its driver stack is broken (seen on dual-boot Windows
    // installs where ICD registration is partial). Blocking at the loader
    // layer makes ggml-vulkan see zero ICDs and skip all device init.
    if crate::gpu_health::previous_crash_detected() {
        let ctx = crate::gpu_health::crash_context().unwrap_or_else(|| "unknown".to_string());
        let fingerprint = crate::gpu_diag::compute_driver_fingerprint();
        crate::log(&format!(
            "[GPU] Previous process aborted during GPU init (context: {}). \
             Forcing CPU backend for this session and writing sticky \
             known-bad marker (fingerprint: {}) so future cold starts skip \
             the GPU path until drivers change.",
            ctx, fingerprint
        ));
        crate::gpu_health::mark_known_bad(&ctx, &fingerprint);
        crate::gpu_health::clear();
        crate::gpu_diag::disable_vulkan_loader(
            "sentinel: previous process aborted during GPU init",
        );
        return GpuBackendStatus::Unavailable;
    }

    // Escape hatch for debugging / CI: force CPU backend regardless of probe.
    if std::env::var("DIMMY_FORCE_CPU").is_ok() {
        crate::log("[GPU] DIMMY_FORCE_CPU set — forcing CPU backend");
        crate::gpu_diag::disable_vulkan_loader("DIMMY_FORCE_CPU=1");
        return GpuBackendStatus::Unavailable;
    }

    // macOS uses Metal via ggml-metal; there is no separate Vulkan loader to
    // probe and Metal is always available on supported hardware. Trust it.
    #[cfg(target_os = "macos")]
    {
        GpuBackendStatus::Available { device: 0 }
    }
    // Windows / Linux: probe Vulkan.
    #[cfg(not(target_os = "macos"))]
    {
        match probe_vulkan() {
            VulkanProbe::Unusable => {
                crate::log(
                    "[GPU] Vulkan backend is not usable on this machine — falling back to CPU. \
                     (Check that vulkan-1.dll is installed and a recent GPU driver exposes an ICD.)",
                );
                crate::gpu_diag::disable_vulkan_loader("probe_vulkan returned Unusable");
                GpuBackendStatus::Unavailable
            }
            VulkanProbe::Usable {
                discrete_gpu_idx,
                discrete_name,
            } => {
                // Explicit env override always wins over auto-detect.
                if let Ok(val) = std::env::var("GGML_VK_DEVICE") {
                    if let Ok(d) = val.parse::<std::ffi::c_int>() {
                        crate::log(&format!("[GPU] Device override from GGML_VK_DEVICE={}", d));
                        return GpuBackendStatus::Available { device: d };
                    }
                }
                // Pick the device in GGML'S OWN coordinate space — never a raw
                // vkEnumeratePhysicalDevices index, never a hardcoded 0. The raw
                // probe (above) tells us the NAME of the first discrete GPU (by
                // VkPhysicalDeviceType); we then ask ggml-vulkan for its own
                // device list and bridge by name. This is the only stable way
                // across machines: a hardcoded index lands on whatever ggml put
                // there (on a dual-GPU laptop that was the *integrated* GPU,
                // whose tiny shared-memory budget OOM-crashed large models while
                // the discrete card's VRAM sat idle).
                let device = resolve_ggml_device(discrete_gpu_idx, discrete_name.as_deref());
                GpuBackendStatus::Available { device }
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
#[cfg(any(feature = "local-stt", feature = "local-llm"))]
#[derive(Clone, Debug)]
enum VulkanProbe {
    /// Vulkan loader / ICD / device enumeration failed — the backend cannot run.
    Unusable,
    /// Vulkan initialized successfully. `discrete_gpu_idx` is the raw-probe
    /// index of the first DISCRETE_GPU (for logging only); `discrete_name` is
    /// that device's name — the STABLE key we bridge into ggml's own device
    /// enumeration (raw-probe indices and ggml indices live in different
    /// coordinate spaces, so we must match by name, never by index).
    Usable {
        discrete_gpu_idx: Option<std::ffi::c_int>,
        discrete_name: Option<String>,
    },
}

/// Enumerate Vulkan physical devices via raw FFI to vulkan-1.dll.
/// Distinguishes between "Vulkan loader / ICD not usable" and "Vulkan usable but
/// no discrete GPU" — critical for deciding whether to fall back to CPU at the
/// whisper/llama init call site.
#[cfg(not(target_os = "macos"))]
#[cfg(any(feature = "local-stt", feature = "local-llm"))]
fn probe_vulkan() -> VulkanProbe {
    use std::ffi::c_int;

    // Vulkan constants
    const VK_SUCCESS: i32 = 0;
    const VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU: u32 = 2;
    const VK_API_VERSION_1_0: u32 = 1 << 22; // 1.0.0

    // Minimal Vulkan types (only what we need)
    type VkInstance = *mut std::ffi::c_void;
    type VkPhysicalDevice = *mut std::ffi::c_void;

    #[repr(C)]
    struct VkApplicationInfo {
        s_type: u32,
        p_next: *const std::ffi::c_void,
        p_application_name: *const u8,
        application_version: u32,
        p_engine_name: *const u8,
        engine_version: u32,
        api_version: u32,
    }

    #[repr(C)]
    struct VkInstanceCreateInfo {
        s_type: u32,
        p_next: *const std::ffi::c_void,
        flags: u32,
        p_application_info: *const VkApplicationInfo,
        enabled_layer_count: u32,
        pp_enabled_layer_names: *const *const u8,
        enabled_extension_count: u32,
        pp_enabled_extension_names: *const *const u8,
    }

    // VkPhysicalDeviceProperties — we only care about deviceType at offset 8
    #[repr(C)]
    struct VkPhysicalDeviceProperties {
        api_version: u32,
        driver_version: u32,
        vendor_id: u32,
        device_id: u32,
        device_type: u32,
        device_name: [u8; 256],
        _rest: [u8; 1024], // pipeline_cache_uuid + limits + sparse — we don't read these
    }

    // Function pointer types
    type FnCreateInstance = unsafe extern "system" fn(
        *const VkInstanceCreateInfo,
        *const std::ffi::c_void,
        *mut VkInstance,
    ) -> i32;
    type FnDestroyInstance = unsafe extern "system" fn(VkInstance, *const std::ffi::c_void);
    type FnEnumeratePhysicalDevices =
        unsafe extern "system" fn(VkInstance, *mut u32, *mut VkPhysicalDevice) -> i32;
    type FnGetPhysicalDeviceProperties =
        unsafe extern "system" fn(VkPhysicalDevice, *mut VkPhysicalDeviceProperties);

    // macOS is excluded at the cfg level above (Metal handles its own probe).
    #[cfg(target_os = "windows")]
    let lib_name = b"vulkan-1.dll\0";
    #[cfg(target_os = "linux")]
    let lib_name = b"libvulkan.so.1\0";

    let result = std::panic::catch_unwind(|| unsafe {
        #[cfg(target_os = "windows")]
        {
            extern "system" {
                fn LoadLibraryA(name: *const u8) -> *mut std::ffi::c_void;
                fn GetProcAddress(
                    module: *mut std::ffi::c_void,
                    name: *const u8,
                ) -> *mut std::ffi::c_void;
                fn FreeLibrary(module: *mut std::ffi::c_void) -> i32;
            }

            let module = LoadLibraryA(lib_name.as_ptr());
            if module.is_null() {
                return VulkanProbe::Unusable;
            }

            macro_rules! load_fn {
                ($name:expr, $ty:ty) => {{
                    let f = GetProcAddress(module, concat!($name, "\0").as_ptr());
                    if f.is_null() {
                        FreeLibrary(module);
                        return VulkanProbe::Unusable;
                    }
                    std::mem::transmute::<_, $ty>(f)
                }};
            }

            let create_instance: FnCreateInstance = load_fn!("vkCreateInstance", FnCreateInstance);
            let destroy_instance: FnDestroyInstance =
                load_fn!("vkDestroyInstance", FnDestroyInstance);
            let enum_devices: FnEnumeratePhysicalDevices =
                load_fn!("vkEnumeratePhysicalDevices", FnEnumeratePhysicalDevices);
            let get_props: FnGetPhysicalDeviceProperties = load_fn!(
                "vkGetPhysicalDeviceProperties",
                FnGetPhysicalDeviceProperties
            );

            // Create minimal Vulkan instance
            let app_info = VkApplicationInfo {
                s_type: 0, // VK_STRUCTURE_TYPE_APPLICATION_INFO
                p_next: std::ptr::null(),
                p_application_name: c"dimmy-gpu-probe".as_ptr().cast(),
                application_version: 0,
                p_engine_name: std::ptr::null(),
                engine_version: 0,
                api_version: VK_API_VERSION_1_0,
            };
            let create_info = VkInstanceCreateInfo {
                s_type: 1, // VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO
                p_next: std::ptr::null(),
                flags: 0,
                p_application_info: &app_info,
                enabled_layer_count: 0,
                pp_enabled_layer_names: std::ptr::null(),
                enabled_extension_count: 0,
                pp_enabled_extension_names: std::ptr::null(),
            };

            let mut instance: VkInstance = std::ptr::null_mut();
            if create_instance(&create_info, std::ptr::null(), &mut instance) != VK_SUCCESS {
                FreeLibrary(module);
                return VulkanProbe::Unusable;
            }

            // Enumerate physical devices
            let mut count: u32 = 0;
            if enum_devices(instance, &mut count, std::ptr::null_mut()) != VK_SUCCESS || count == 0
            {
                destroy_instance(instance, std::ptr::null());
                FreeLibrary(module);
                return VulkanProbe::Unusable;
            }

            let mut devices = vec![std::ptr::null_mut(); count as usize];
            if enum_devices(instance, &mut count, devices.as_mut_ptr()) != VK_SUCCESS {
                destroy_instance(instance, std::ptr::null());
                FreeLibrary(module);
                return VulkanProbe::Unusable;
            }

            // Find first discrete GPU (index for logging + name for the
            // ggml bridge).
            let mut result: Option<c_int> = None;
            let mut discrete_name: Option<String> = None;
            for (i, &dev) in devices.iter().enumerate() {
                let mut props = std::mem::zeroed::<VkPhysicalDeviceProperties>();
                get_props(dev, &mut props);
                let name = std::ffi::CStr::from_ptr(props.device_name.as_ptr() as *const _)
                    .to_string_lossy();
                let type_str = match props.device_type {
                    1 => "Integrated",
                    2 => "Discrete",
                    3 => "Virtual",
                    4 => "CPU",
                    _ => "Other",
                };
                crate::log(&format!(
                    "[LocalSTT] Vulkan device {}: {} ({})",
                    i, name, type_str
                ));

                if props.device_type == VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU && result.is_none() {
                    result = Some(i as c_int);
                    discrete_name = Some(name.into_owned());
                }
            }

            destroy_instance(instance, std::ptr::null());
            FreeLibrary(module);
            VulkanProbe::Usable {
                discrete_gpu_idx: result,
                discrete_name,
            }
        }

        #[cfg(target_os = "linux")]
        {
            // On Linux, use dlopen/dlsym (libc)
            extern "C" {
                fn dlopen(filename: *const u8, flags: i32) -> *mut std::ffi::c_void;
                fn dlsym(handle: *mut std::ffi::c_void, symbol: *const u8)
                    -> *mut std::ffi::c_void;
                fn dlclose(handle: *mut std::ffi::c_void) -> i32;
            }
            const RTLD_LAZY: i32 = 1;

            let module = dlopen(lib_name.as_ptr(), RTLD_LAZY);
            if module.is_null() {
                return VulkanProbe::Unusable;
            }

            macro_rules! load_fn {
                ($name:expr, $ty:ty) => {{
                    let f = dlsym(module, concat!($name, "\0").as_ptr());
                    if f.is_null() {
                        dlclose(module);
                        return VulkanProbe::Unusable;
                    }
                    std::mem::transmute::<_, $ty>(f)
                }};
            }

            let create_instance: FnCreateInstance = load_fn!("vkCreateInstance", FnCreateInstance);
            let destroy_instance: FnDestroyInstance =
                load_fn!("vkDestroyInstance", FnDestroyInstance);
            let enum_devices: FnEnumeratePhysicalDevices =
                load_fn!("vkEnumeratePhysicalDevices", FnEnumeratePhysicalDevices);
            let get_props: FnGetPhysicalDeviceProperties = load_fn!(
                "vkGetPhysicalDeviceProperties",
                FnGetPhysicalDeviceProperties
            );

            let app_info = VkApplicationInfo {
                s_type: 0,
                p_next: std::ptr::null(),
                p_application_name: c"dimmy-gpu-probe".as_ptr().cast(),
                application_version: 0,
                p_engine_name: std::ptr::null(),
                engine_version: 0,
                api_version: VK_API_VERSION_1_0,
            };
            let create_info = VkInstanceCreateInfo {
                s_type: 1,
                p_next: std::ptr::null(),
                flags: 0,
                p_application_info: &app_info,
                enabled_layer_count: 0,
                pp_enabled_layer_names: std::ptr::null(),
                enabled_extension_count: 0,
                pp_enabled_extension_names: std::ptr::null(),
            };

            let mut instance: VkInstance = std::ptr::null_mut();
            if create_instance(&create_info, std::ptr::null(), &mut instance) != VK_SUCCESS {
                dlclose(module);
                return VulkanProbe::Unusable;
            }

            let mut count: u32 = 0;
            if enum_devices(instance, &mut count, std::ptr::null_mut()) != VK_SUCCESS || count == 0
            {
                destroy_instance(instance, std::ptr::null());
                dlclose(module);
                return VulkanProbe::Unusable;
            }

            let mut devices = vec![std::ptr::null_mut(); count as usize];
            if enum_devices(instance, &mut count, devices.as_mut_ptr()) != VK_SUCCESS {
                destroy_instance(instance, std::ptr::null());
                dlclose(module);
                return VulkanProbe::Unusable;
            }

            let mut result: Option<c_int> = None;
            let mut discrete_name: Option<String> = None;
            for (i, &dev) in devices.iter().enumerate() {
                let mut props = std::mem::zeroed::<VkPhysicalDeviceProperties>();
                get_props(dev, &mut props);
                let name = std::ffi::CStr::from_ptr(props.device_name.as_ptr() as *const _)
                    .to_string_lossy();
                let type_str = match props.device_type {
                    1 => "Integrated",
                    2 => "Discrete",
                    3 => "Virtual",
                    4 => "CPU",
                    _ => "Other",
                };
                crate::log(&format!(
                    "[LocalSTT] Vulkan device {}: {} ({})",
                    i, name, type_str
                ));

                if props.device_type == VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU && result.is_none() {
                    result = Some(i as c_int);
                    discrete_name = Some(name.into_owned());
                }
            }

            destroy_instance(instance, std::ptr::null());
            dlclose(module);
            VulkanProbe::Usable {
                discrete_gpu_idx: result,
                discrete_name,
            }
        }
    });

    result.unwrap_or(VulkanProbe::Unusable)
}

/// ggml-vulkan's OWN device enumeration: (ggml_index, name, total_mb). The
/// index is the coordinate space `WhisperContextParameters::gpu_device`
/// expects — distinct from the raw `vkEnumeratePhysicalDevices` order. Linked
/// only when whisper is built with the Vulkan backend; `catch_unwind` guards
/// the FFI boundary so a probe failure degrades to "empty list" (→ device 0).
#[cfg(not(target_os = "macos"))]
#[cfg(any(feature = "local-stt", feature = "local-llm"))]
#[cfg(feature = "local-stt-vulkan")]
fn ggml_vulkan_devices() -> Vec<(std::ffi::c_int, String, u64)> {
    use std::ffi::{c_char, c_int};
    extern "C" {
        fn ggml_backend_vk_get_device_count() -> c_int;
        fn ggml_backend_vk_get_device_description(
            device: c_int,
            description: *mut c_char,
            description_size: usize,
        );
        fn ggml_backend_vk_get_device_memory(device: c_int, free: *mut usize, total: *mut usize);
    }
    let probe = std::panic::catch_unwind(|| unsafe {
        let count = ggml_backend_vk_get_device_count();
        let mut out: Vec<(c_int, String, u64)> = Vec::new();
        for i in 0..count {
            let mut buf = [0u8; 256];
            ggml_backend_vk_get_device_description(i, buf.as_mut_ptr() as *mut c_char, buf.len());
            let name = std::ffi::CStr::from_ptr(buf.as_ptr() as *const c_char)
                .to_string_lossy()
                .trim()
                .to_string();
            let mut free: usize = 0;
            let mut total: usize = 0;
            ggml_backend_vk_get_device_memory(i, &mut free, &mut total);
            out.push((i, name, (total / (1024 * 1024)) as u64));
        }
        out
    });
    probe.unwrap_or_default()
}

#[cfg(not(target_os = "macos"))]
#[cfg(any(feature = "local-stt", feature = "local-llm"))]
#[cfg(not(feature = "local-stt-vulkan"))]
fn ggml_vulkan_devices() -> Vec<(std::ffi::c_int, String, u64)> {
    Vec::new()
}

/// Pure name-bridge between the raw Vulkan probe (which knows, via
/// `VkPhysicalDeviceType`, *which* device is discrete) and ggml's own device
/// list (which owns the index whisper wants). Matching is tolerant — exact,
/// then case-insensitive substring either direction — because both names come
/// from the same driver string but may differ in trailing NUL/whitespace.
/// Pure + hardware-free so it is unit-tested across GPU layouts.
#[cfg(not(target_os = "macos"))]
#[cfg(any(feature = "local-stt", feature = "local-llm"))]
fn match_ggml_device_index(
    discrete_name: &str,
    ggml_devices: &[(std::ffi::c_int, String)],
) -> Option<std::ffi::c_int> {
    let want = discrete_name.trim();
    if want.is_empty() {
        return None;
    }
    for (idx, name) in ggml_devices {
        if name.trim() == want {
            return Some(*idx);
        }
    }
    let wl = want.to_lowercase();
    for (idx, name) in ggml_devices {
        let nl = name.trim().to_lowercase();
        if nl == wl || nl.contains(&wl) || wl.contains(&nl) {
            return Some(*idx);
        }
    }
    None
}

/// Decide which ggml-vulkan device index to hand to whisper. Prefers the
/// discrete GPU (resolved by name in ggml's coordinate space); falls back to
/// device 0 when there is no discrete GPU (integrated-only machine) or when
/// ggml can't see the discrete one. Logs the full mapping so a future dual-GPU
/// mis-selection is diagnosable straight from dimmy.log.
#[cfg(not(target_os = "macos"))]
#[cfg(any(feature = "local-stt", feature = "local-llm"))]
fn resolve_ggml_device(
    discrete_gpu_idx: Option<std::ffi::c_int>,
    discrete_name: Option<&str>,
) -> std::ffi::c_int {
    let devices = ggml_vulkan_devices();
    if devices.is_empty() {
        crate::log(&format!(
            "[GPU] ggml device list empty — using device 0 (raw-probe discrete_idx={:?}, name={:?})",
            discrete_gpu_idx, discrete_name
        ));
        return 0;
    }
    for (idx, name, total_mb) in &devices {
        crate::log(&format!(
            "[GPU] ggml device {}: {} ({} MB total)",
            idx, name, total_mb
        ));
    }
    let pairs: Vec<(std::ffi::c_int, String)> =
        devices.iter().map(|(i, n, _)| (*i, n.clone())).collect();
    match discrete_name {
        Some(name) => match match_ggml_device_index(name, &pairs) {
            Some(idx) => {
                crate::log(&format!(
                    "[GPU] selected ggml device {} (discrete GPU '{}')",
                    idx, name
                ));
                idx
            }
            None => {
                crate::log(&format!(
                    "[GPU] discrete GPU '{}' not in ggml list — using device 0 (integrated/fallback)",
                    name
                ));
                0
            }
        },
        None => {
            crate::log("[GPU] no discrete GPU — using ggml device 0 (integrated/only)");
            0
        }
    }
}

// ── WhisperContext cache ─────────────────────────────────────────
//
// Loading a whisper model into VRAM takes 2-5 seconds for large models.
// We cache the WhisperContext globally and reuse it across transcriptions.
// The cache is invalidated when the model changes.

#[cfg(feature = "local-stt")]
mod whisper_cache {
    use std::path::PathBuf;
    use std::sync::Mutex;
    use whisper_rs::{WhisperContext, WhisperContextParameters};

    struct CachedModel {
        // Retained for the lifetime of the cache entry so the loaded model
        // unambiguously outlives the reused `state`. Not read after the
        // state is built (WhisperState owns an Arc to the inner context),
        // but we keep the wrapper as belt-and-suspenders against a
        // use-after-free if that ownership ever changes upstream.
        #[allow(dead_code)]
        ctx: WhisperContext,
        // Inference state is created ONCE per model load and reused across
        // every whisper_full call. In whisper-rs 0.16 WhisperState is owned
        // + Send + Sync (it holds an Arc to the inner context, no lifetime),
        // so it lives in the cache next to ctx. Re-creating it per chunk
        // re-ran whisper_backend_init_gpu and re-allocated ~450 MB of compute
        // buffers on every call — pure overhead, and the exact site that
        // aborted GPU init under sustained meeting re-transcription.
        state: whisper_rs::WhisperState,
        model_path: PathBuf,
    }

    // WhisperContext is Send (C pointer with proper cleanup) but not Sync by default.
    // We protect access with a Mutex, so only one thread uses it at a time.
    unsafe impl Send for CachedModel {}

    static CACHE: Mutex<Option<CachedModel>> = Mutex::new(None);

    /// Load model if needed, run inference, return text. All under one lock.
    pub fn transcribe(
        model_path: &std::path::Path,
        samples: &[f32],
        language: &str,
        prompt: &str,
    ) -> Result<String, crate::error::TranscribeError> {
        use std::ffi::c_int;
        use whisper_rs::{FullParams, SamplingStrategy};

        let mut guard = CACHE.lock().map_err(|e| {
            crate::error::TranscribeError::LocalModel(format!("cache lock poisoned: {}", e))
        })?;

        // ── Load or reuse cached model ──────────────────────────
        let needs_reload = match &*guard {
            Some(cached) => cached.model_path != model_path,
            None => true,
        };

        if needs_reload {
            crate::log(&format!(
                "[LocalSTT] Loading model into cache: {}",
                model_path.display()
            ));
            let mut ctx_params = WhisperContextParameters::default();
            let mut using_gpu = false;

            match super::gpu_backend_status() {
                super::GpuBackendStatus::Available { device } => {
                    ctx_params.use_gpu(true);
                    ctx_params.gpu_device(device);
                    using_gpu = true;
                    crate::log(&format!("[LocalSTT] GPU backend: device {}", device));
                }
                super::GpuBackendStatus::Unavailable => {
                    ctx_params.use_gpu(false);
                    crate::log("[LocalSTT] GPU backend unavailable — loading model on CPU");
                }
            }

            // Crash-recovery sentinel: ggml-vulkan can abort the whole process
            // when the GPU path fails inside the C++ layer. Writing a sentinel
            // file before the call lets the NEXT run detect the crash and fall
            // back to CPU. The sentinel is cleared whether init succeeds or
            // returns a Rust error — only a hard abort leaves it behind.
            if using_gpu {
                crate::gpu_health::mark_begin(&format!("whisper_load: {}", model_path.display()));
            }
            let ctx_result = WhisperContext::new_with_params(model_path, ctx_params);
            let ctx = match ctx_result {
                Ok(c) => c,
                Err(e) => {
                    if using_gpu {
                        crate::gpu_health::mark_end();
                    }
                    return Err(crate::error::TranscribeError::LocalModel(format!(
                        "failed to load model: {}",
                        e
                    )));
                }
            };
            // Create the inference state ONCE, inside the same GPU
            // crash-recovery window: create_state is where
            // whisper_backend_init_gpu + the ~450 MB compute-buffer
            // allocations happen, so the sentinel must cover it too. The
            // state is then reused for every chunk (see transcribe loop).
            crate::log("[LocalSTT] Creating inference state (once, reused across chunks)");
            let state_result = ctx.create_state();
            if using_gpu {
                crate::gpu_health::mark_end();
            }
            let state = state_result.map_err(|e| {
                crate::log(&format!("[LocalSTT] create_state returned error: {}", e));
                crate::error::TranscribeError::LocalModel(format!("failed to create state: {}", e))
            })?;
            *guard = Some(CachedModel {
                ctx,
                state,
                model_path: model_path.to_path_buf(),
            });
            crate::log("[LocalSTT] Model + state cached successfully");
        } else {
            crate::log("[LocalSTT] Using cached model (skip VRAM reload)");
        }

        // Reuse the cached state — created once at model load. No per-chunk
        // create_state(), so no repeated whisper_backend_init_gpu and no
        // ~450 MB compute-buffer churn (that re-init was the GPU-abort site).
        let cached = guard.as_mut().expect("cache must be populated after load");
        let state = &mut cached.state;

        // ── Run inference on the reused state ────────────────────
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        // The state is shared across chunks, so suppress cross-call token
        // history: each chunk must transcribe independently, exactly as a
        // freshly-created state did. (The initial_prompt dict-biasing below
        // still applies per call.)
        params.set_no_context(true);

        // Inject the composed user prompt + dict (Wispr Flow-style
        // vocabulary biasing). Whisper.cpp treats `initial_prompt` as
        // the preceding-context for the model — when present, recognition
        // is biased toward those words. Empty string is the natural
        // no-op (skip set_initial_prompt entirely so the binding doesn't
        // emit "<|prev|>" framing for nothing). The 224-token Whisper
        // limit is upstream's responsibility; we just forward the
        // composed string straight through.
        if !prompt.trim().is_empty() {
            params.set_initial_prompt(prompt);
            crate::log(&format!(
                "[DictBias] provider=whisper_local prompt_chars={}",
                prompt.len()
            ));
        }

        if !language.is_empty() {
            params.set_language(Some(language));
        } else {
            params.set_detect_language(true);
        }

        let n_threads: c_int = std::thread::available_parallelism()
            .map(|n| n.get().min(4) as c_int)
            .unwrap_or(2);
        params.set_n_threads(n_threads);

        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        // Suppress the non-speech token class. Free, and it is the cheap half
        // of the anti-hallucination pair with the no-speech filter below.
        params.set_suppress_nst(true);

        // Intentionally not calling `set_single_segment(true)`. It interacts badly with
        // language detection: whisper returns Ok with zero segments even when the audio is
        // clear speech and the detected language is confident (observed p=0.97 Italian,
        // 5 s of audio, 0 chars output). Letting whisper chunk normally produces segments.
        let single_segment = false;

        crate::log(&format!(
            "[LocalSTT] Running whisper_full (n_threads={}, single_segment={}, samples={})",
            n_threads,
            single_segment,
            samples.len()
        ));
        let full_result = state.full(params, samples);
        match &full_result {
            Ok(_) => crate::log("[LocalSTT] whisper_full returned Ok"),
            Err(e) => crate::log(&format!("[LocalSTT] whisper_full returned Err: {}", e)),
        }
        full_result.map_err(|e| {
            crate::error::TranscribeError::LocalModel(format!("whisper inference failed: {}", e))
        })?;

        let n_segments = state.full_n_segments();
        crate::log(&format!("[LocalSTT] Extracting {} segment(s)", n_segments));
        let mut text = String::new();
        for i in 0..n_segments {
            let segment = state.get_segment(i).ok_or_else(|| {
                crate::error::TranscribeError::LocalModel(format!("segment {} out of bounds", i))
            })?;
            let seg_text = segment.to_str().map_err(|e| {
                crate::error::TranscribeError::LocalModel(format!(
                    "failed to read segment {}: {}",
                    i, e
                ))
            })?;
            // NOTE: `segment.no_speech_probability()` is deliberately NOT used
            // as a filter. It is the standard second net (OpenAI's reference
            // decoder and faster-whisper both threshold it at 0.6), but it is
            // useless on large-v3-turbo: measured over 45 segments of two real
            // meetings on 2026-07-31 it never exceeded 0.00002, and the
            // hallucinated "Grazie a tutti" segments reported exactly 0.00000.
            // Whisper is most confident precisely when it is inventing, so a
            // confidence filter cannot see hallucinations. Silence has to be
            // removed from the AUDIO before it gets here — see
            // `preprocess::process_chunk_vad_only`.
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(seg_text.trim());
        }

        let final_text = text.trim().to_string();
        crate::log(&format!(
            "[LocalSTT] Inference complete — {} chars",
            final_text.len()
        ));
        Ok(final_text)
    }

    /// Clear the cached model (e.g. on shutdown or model change).
    pub fn clear() {
        if let Ok(mut guard) = CACHE.lock() {
            if guard.is_some() {
                crate::log("[LocalSTT] Clearing model cache");
            }
            *guard = None;
        }
    }
}

/// Clear the whisper model cache (call on shutdown or model change).
#[cfg(feature = "local-stt")]
pub fn clear_model_cache() {
    whisper_cache::clear();
}

#[cfg(not(feature = "local-stt"))]
pub fn clear_model_cache() {}

// ── Local transcription (feature-gated) ───────────────────────────

#[cfg(feature = "local-stt")]
pub fn transcribe_local(
    model_file: &Path,
    samples: &[f32], // 16 kHz mono
    language: &str,
    prompt: &str,
) -> Result<String, TranscribeError> {
    // ── Precondition assertions ──────────────────────────────────
    assert!(!samples.is_empty(), "samples must not be empty");
    assert!(
        samples.iter().all(|s| s.is_finite()),
        "all samples must be finite (no NaN/Inf)"
    );

    if !model_file.is_file() {
        return Err(TranscribeError::LocalModel(format!(
            "model file not found: {}",
            model_file.display()
        )));
    }

    // ── Transcribe with cached WhisperContext ────────────────────
    // whisper runs its heavy matmuls on the GPU, but the CPU side is still
    // n_threads wide and still gets demoted with the rest of the process:
    // 3 s throttled against 2 s exempt on the same 25 s recording. Smaller
    // than the denoise win, real enough to take. See `win_qos`.
    let _no_throttle = crate::win_qos::NoThrottle::for_local_inference();
    let result = whisper_cache::transcribe(model_file, samples, language, prompt)?;

    // ── Postcondition ────────────────────────────────────────────
    if result.is_empty() {
        return Err(TranscribeError::Empty);
    }

    Ok(result)
}

/// Stub when `local-stt` feature is disabled.
#[cfg(not(feature = "local-stt"))]
pub fn transcribe_local(
    _model_file: &Path,
    _samples: &[f32],
    _language: &str,
    _prompt: &str,
) -> Result<String, TranscribeError> {
    Err(TranscribeError::LocalModel(
        "local STT not available: compile with `local-stt` feature".to_string(),
    ))
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_directory_is_valid() {
        let dir = model_directory();
        let s = dir.to_str().unwrap();
        assert!(s.contains("dimmy"), "path should contain 'dimmy': {}", s);
        assert!(s.contains("models"), "path should contain 'models': {}", s);
    }

    #[test]
    fn model_exists_false_for_missing() {
        assert!(!model_exists("nonexistent-model.bin"));
    }

    // ── ggml device name-bridge (the dual-GPU OOM fix) ──────────────
    // These prove the discrete-GPU selection is correct across GPU layouts,
    // not just the one laptop it was found on. The bridge maps the discrete
    // device NAME (from the raw Vulkan probe, by VkPhysicalDeviceType) to
    // ggml's OWN device index — the only stable cross-machine mapping.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn ggml_match_dual_gpu_picks_discrete_not_index_zero() {
        // The exact failing case: ggml lists Intel at 0, NVIDIA at 1. A
        // hardcoded `0` lands on the integrated GPU (the OOM bug); matching by
        // name must return 1.
        let ggml = vec![
            (0, "Intel(R) UHD Graphics".to_string()),
            (1, "NVIDIA T600 Laptop GPU".to_string()),
        ];
        assert_eq!(
            match_ggml_device_index("NVIDIA T600 Laptop GPU", &ggml),
            Some(1)
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn ggml_match_single_discrete_at_zero() {
        // Single-GPU desktop: discrete is the only device.
        let ggml = vec![(0, "NVIDIA GeForce RTX 4070".to_string())];
        assert_eq!(
            match_ggml_device_index("NVIDIA GeForce RTX 4070", &ggml),
            Some(0)
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn ggml_match_tolerant_to_whitespace_and_case() {
        let ggml = vec![
            (0, "Intel(R) UHD Graphics".to_string()),
            (1, "  AMD Radeon RX 7900 XTX  ".to_string()),
        ];
        assert_eq!(
            match_ggml_device_index("AMD Radeon RX 7900 XTX", &ggml),
            Some(1)
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn ggml_match_none_when_discrete_absent_from_ggml() {
        // Discrete exists in the raw probe but ggml can't see it (driver
        // exposes only the iGPU to Vulkan) → no match → caller falls back to 0.
        let ggml = vec![(0, "Intel(R) UHD Graphics".to_string())];
        assert_eq!(
            match_ggml_device_index("NVIDIA T600 Laptop GPU", &ggml),
            None
        );
        assert_eq!(match_ggml_device_index("", &ggml), None);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn ggml_match_multi_discrete_picks_named_one() {
        // Two discretes: the probe names the first one; bridge returns its
        // exact ggml index regardless of position.
        let ggml = vec![
            (0, "Intel(R) UHD Graphics".to_string()),
            (1, "NVIDIA RTX A2000".to_string()),
            (2, "NVIDIA T600 Laptop GPU".to_string()),
        ];
        assert_eq!(
            match_ggml_device_index("NVIDIA T600 Laptop GPU", &ggml),
            Some(2)
        );
    }

    #[test]
    fn available_models_are_valid() {
        for model in AVAILABLE_MODELS {
            assert!(!model.name.is_empty(), "model name must not be empty");
            assert!(
                model.filename.ends_with(".bin"),
                "model filename must end with .bin: {}",
                model.filename
            );
            assert!(
                model.size_mb > 0,
                "model size must be positive: {}",
                model.name
            );
            assert!(
                !model.description.is_empty(),
                "model description must not be empty: {}",
                model.name
            );
            if let Some(url) = model.url {
                assert!(
                    url.starts_with("https://"),
                    "custom URL must be HTTPS: {} ({})",
                    url,
                    model.name
                );
                assert!(
                    url.contains(model.filename),
                    "custom URL must contain filename: {} ({})",
                    url,
                    model.name
                );
            }
        }
    }

    #[test]
    fn no_duplicate_filenames() {
        let mut seen = std::collections::HashSet::new();
        for model in AVAILABLE_MODELS {
            assert!(
                seen.insert(model.filename),
                "duplicate filename in AVAILABLE_MODELS: {}",
                model.filename
            );
        }
    }

    #[test]
    fn default_model_is_in_available_list() {
        assert!(
            AVAILABLE_MODELS.iter().any(|m| m.filename == DEFAULT_MODEL),
            "DEFAULT_MODEL '{}' must appear in AVAILABLE_MODELS",
            DEFAULT_MODEL
        );
    }

    #[test]
    fn model_path_contains_filename() {
        let p = model_path("ggml-tiny-q8_0.bin");
        assert!(
            p.ends_with("ggml-tiny-q8_0.bin"),
            "model_path should end with filename: {}",
            p.display()
        );
        assert!(
            p.to_str().unwrap().contains("dimmy"),
            "model_path should be under dimmy dir"
        );
    }

    #[cfg(feature = "local-stt")]
    #[test]
    fn transcribe_local_rejects_missing_model() {
        let samples = vec![0.0f32; 16000];
        let result = transcribe_local(Path::new("/nonexistent/model.bin"), &samples, "en", "");
        assert!(result.is_err());
        if let Err(TranscribeError::LocalModel(msg)) = result {
            assert!(
                msg.contains("not found"),
                "error should mention 'not found': {}",
                msg
            );
        } else {
            panic!("Expected LocalModel error");
        }
    }

    #[cfg(not(feature = "local-stt"))]
    #[test]
    fn transcribe_local_stub_returns_error() {
        let samples = vec![0.0f32; 16000];
        let result = transcribe_local(Path::new("/any/model.bin"), &samples, "en");
        assert!(result.is_err());
        if let Err(TranscribeError::LocalModel(msg)) = result {
            assert!(
                msg.contains("not available"),
                "stub error should mention 'not available': {}",
                msg
            );
        } else {
            panic!("Expected LocalModel error from stub");
        }
    }

    #[test]
    fn clear_model_cache_does_not_panic() {
        // Should be safe to call even when no model is cached
        clear_model_cache();
        clear_model_cache(); // idempotent
    }

    #[cfg(feature = "local-stt")]
    #[test]
    fn cache_rejects_missing_model() {
        // Trying to cache a non-existent model should fail gracefully
        clear_model_cache();
        let result = transcribe_local(
            Path::new("/nonexistent/model.bin"),
            &[0.1f32; 16000],
            "en",
            "",
        );
        assert!(result.is_err());
    }
}
