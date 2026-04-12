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

/// Returns `<data_dir>/dimmy/models` (e.g. `~/Library/Application Support/dimmy/models`).
pub fn model_directory() -> PathBuf {
    let base = dirs::data_dir().expect("data_dir must be available on all supported platforms");
    base.join("dimmy").join("models")
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

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| TranscribeError::LocalModel(format!("download request failed: {}", e)))?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        let body_trunc = if body.len() > 200 {
            &body[..200]
        } else {
            &body
        };
        return Err(TranscribeError::LocalModel(format!(
            "download failed: HTTP {} — {}",
            status, body_trunc
        )));
    }

    let total_bytes = resp.content_length().unwrap_or(0);
    let part_path = dir.join(format!("{}.part", filename));

    let mut file = std::fs::File::create(&part_path).map_err(|e| {
        TranscribeError::LocalModel(format!(
            "cannot create temp file {}: {}",
            part_path.display(),
            e
        ))
    })?;

    let mut downloaded: u64 = 0;

    // Stream the response body using chunk() (no `stream` feature needed).
    use std::io::Write;
    let mut resp = resp;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| TranscribeError::LocalModel(format!("download stream error: {}", e)))?
    {
        file.write_all(&chunk)
            .map_err(|e| TranscribeError::LocalModel(format!("write error: {}", e)))?;
        downloaded += chunk.len() as u64;
        on_progress(downloaded, total_bytes);
    }

    drop(file); // flush & close before rename

    // Atomic rename: .part → final
    std::fs::rename(&part_path, &dest).map_err(|e| {
        TranscribeError::LocalModel(format!(
            "rename {} → {} failed: {}",
            part_path.display(),
            dest.display(),
            e
        ))
    })?;

    crate::log(&format!(
        "[LocalSTT] Download complete: {} ({} bytes)",
        dest.display(),
        downloaded
    ));
    assert!(dest.is_file(), "model file must exist after download");

    Ok(dest)
}

// ── Vulkan GPU device selection ──────────────────────────────────
//
// On multi-GPU systems (e.g. Optimus laptops with Intel iGPU + NVIDIA dGPU),
// ggml_vulkan defaults to device 0, which is often the integrated GPU.
// Intel iGPU Vulkan compute is unstable and crashes during whisper inference.
// We enumerate Vulkan physical devices and prefer the first discrete GPU.

/// Returns the Vulkan device index to use for GPU inference.
/// Priority: GGML_VK_DEVICE env var > first discrete GPU > 0.
#[cfg(feature = "local-stt")]
fn preferred_gpu_device() -> std::ffi::c_int {
    // 1. Env var override — power users / CI can force a specific device
    if let Ok(val) = std::env::var("GGML_VK_DEVICE") {
        if let Ok(d) = val.parse::<std::ffi::c_int>() {
            crate::log(&format!(
                "[LocalSTT] GPU device override from GGML_VK_DEVICE={}",
                d
            ));
            return d;
        }
    }

    // 2. Auto-detect: enumerate Vulkan devices, prefer discrete GPU
    match detect_discrete_gpu() {
        Some(idx) => {
            crate::log(&format!(
                "[LocalSTT] Auto-detected discrete GPU at device {}",
                idx
            ));
            idx
        }
        None => {
            crate::log("[LocalSTT] No discrete GPU found, using device 0");
            0
        }
    }
}

/// Enumerate Vulkan physical devices via raw FFI to vulkan-1.dll.
/// Returns the index of the first discrete GPU, or None.
/// Zero new dependencies — vulkan-1.dll is already loaded by ggml_vulkan.
#[cfg(feature = "local-stt")]
fn detect_discrete_gpu() -> Option<std::ffi::c_int> {
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

    // macOS uses Metal, not Vulkan — no device enumeration needed
    #[cfg(target_os = "macos")]
    {
        return None;
    }

    #[cfg(not(target_os = "macos"))]
    {
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
                    return None;
                }

                macro_rules! load_fn {
                    ($name:expr, $ty:ty) => {{
                        let f = GetProcAddress(module, concat!($name, "\0").as_ptr());
                        if f.is_null() {
                            FreeLibrary(module);
                            return None;
                        }
                        std::mem::transmute::<_, $ty>(f)
                    }};
                }

                let create_instance: FnCreateInstance =
                    load_fn!("vkCreateInstance", FnCreateInstance);
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
                    return None;
                }

                // Enumerate physical devices
                let mut count: u32 = 0;
                if enum_devices(instance, &mut count, std::ptr::null_mut()) != VK_SUCCESS
                    || count == 0
                {
                    destroy_instance(instance, std::ptr::null());
                    FreeLibrary(module);
                    return None;
                }

                let mut devices = vec![std::ptr::null_mut(); count as usize];
                if enum_devices(instance, &mut count, devices.as_mut_ptr()) != VK_SUCCESS {
                    destroy_instance(instance, std::ptr::null());
                    FreeLibrary(module);
                    return None;
                }

                // Find first discrete GPU
                let mut result: Option<c_int> = None;
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

                    if props.device_type == VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU && result.is_none()
                    {
                        result = Some(i as c_int);
                    }
                }

                destroy_instance(instance, std::ptr::null());
                FreeLibrary(module);
                result
            }

            #[cfg(target_os = "linux")]
            {
                // On Linux, use dlopen/dlsym (libc)
                extern "C" {
                    fn dlopen(filename: *const u8, flags: i32) -> *mut std::ffi::c_void;
                    fn dlsym(
                        handle: *mut std::ffi::c_void,
                        symbol: *const u8,
                    ) -> *mut std::ffi::c_void;
                    fn dlclose(handle: *mut std::ffi::c_void) -> i32;
                }
                const RTLD_LAZY: i32 = 1;

                let module = dlopen(lib_name.as_ptr(), RTLD_LAZY);
                if module.is_null() {
                    return None;
                }

                macro_rules! load_fn {
                    ($name:expr, $ty:ty) => {{
                        let f = dlsym(module, concat!($name, "\0").as_ptr());
                        if f.is_null() {
                            dlclose(module);
                            return None;
                        }
                        std::mem::transmute::<_, $ty>(f)
                    }};
                }

                let create_instance: FnCreateInstance =
                    load_fn!("vkCreateInstance", FnCreateInstance);
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
                    return None;
                }

                let mut count: u32 = 0;
                if enum_devices(instance, &mut count, std::ptr::null_mut()) != VK_SUCCESS
                    || count == 0
                {
                    destroy_instance(instance, std::ptr::null());
                    dlclose(module);
                    return None;
                }

                let mut devices = vec![std::ptr::null_mut(); count as usize];
                if enum_devices(instance, &mut count, devices.as_mut_ptr()) != VK_SUCCESS {
                    destroy_instance(instance, std::ptr::null());
                    dlclose(module);
                    return None;
                }

                let mut result: Option<c_int> = None;
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

                    if props.device_type == VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU && result.is_none()
                    {
                        result = Some(i as c_int);
                    }
                }

                destroy_instance(instance, std::ptr::null());
                dlclose(module);
                result
            }
        });

        result.unwrap_or(None)
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
        ctx: WhisperContext,
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

            let gpu_device = super::preferred_gpu_device();
            ctx_params.gpu_device(gpu_device);
            crate::log(&format!("[LocalSTT] Using GPU device: {}", gpu_device));
            let ctx = WhisperContext::new_with_params(model_path, ctx_params).map_err(|e| {
                crate::error::TranscribeError::LocalModel(format!("failed to load model: {}", e))
            })?;
            *guard = Some(CachedModel {
                ctx,
                model_path: model_path.to_path_buf(),
            });
            crate::log("[LocalSTT] Model cached successfully");
        } else {
            crate::log("[LocalSTT] Using cached model (skip VRAM reload)");
        }

        let cached = guard.as_ref().expect("cache must be populated after load");

        // ── Create state + run inference ─────────────────────────
        let mut state = cached.ctx.create_state().map_err(|e| {
            crate::error::TranscribeError::LocalModel(format!("failed to create state: {}", e))
        })?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });

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

        const SAMPLES_30S: usize = 30 * 16_000;
        if samples.len() < SAMPLES_30S {
            params.set_single_segment(true);
        }

        state.full(params, samples).map_err(|e| {
            crate::error::TranscribeError::LocalModel(format!("whisper inference failed: {}", e))
        })?;

        let n_segments = state.full_n_segments();
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
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(seg_text.trim());
        }

        Ok(text.trim().to_string())
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
    let result = whisper_cache::transcribe(model_file, samples, language)?;

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
        let result = transcribe_local(Path::new("/nonexistent/model.bin"), &samples, "en");
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
        let result = transcribe_local(Path::new("/nonexistent/model.bin"), &[0.1f32; 16000], "en");
        assert!(result.is_err());
    }
}
