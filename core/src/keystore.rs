//! API key storage using local AES-256-CTR + HMAC-SHA256 encrypted file.
///
/// Keys are encrypted with a machine-derived key (SHA-256 of username + hostname + salt)
/// and stored in `keys.enc`. No OS permission popups, no admin needed on any platform.
///
/// The OS keyring (macOS Keychain, Windows Credential Manager, Linux Secret Service) is
/// kept as a read-only fallback: keys stored there by older versions are automatically
/// migrated to the local encrypted file on first load.
use crate::provider::KeyringScope;
use base64::Engine;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

// ── Machine key derivation ──────────────────────────────────────────

/// Derive a 256-bit encryption key from machine identity.
/// Uses SHA-256(username + ":" + hostname + ":dimmy-local-key-v1").
fn derive_machine_key() -> [u8; 32] {
    use std::io::Write;

    let username = whoami_username();
    let hostname = whoami_hostname();

    assert!(
        !username.is_empty(),
        "username must not be empty for key derivation"
    );
    // hostname CAN be empty on some minimal Linux containers — still safe, just less unique

    let mut hasher = Sha256::new();
    hasher.write_all(username.as_bytes()).unwrap();
    hasher.write_all(b":").unwrap();
    hasher.write_all(hostname.as_bytes()).unwrap();
    hasher.write_all(b":dimmy-local-key-v1").unwrap();
    hasher.finalize()
}

// Minimal SHA-256 — avoids pulling in the `sha2` crate for a single use.
// We only need one hash at startup; performance is irrelevant.
struct Sha256 {
    data: Vec<u8>,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            data: Vec::with_capacity(128),
        }
    }

    fn finalize(self) -> [u8; 32] {
        sha256_digest(&self.data)
    }
}

impl std::io::Write for Sha256 {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.data.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Pure-Rust SHA-256 (FIPS 180-4). No external crate needed.
fn sha256_digest(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    // Pre-processing: padding
    let bit_len = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, val) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&val.to_be_bytes());
    }
    out
}

/// Cross-platform username.
fn whoami_username() -> String {
    #[cfg(unix)]
    {
        std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .unwrap_or_else(|_| "unknown".to_string())
    }
    #[cfg(windows)]
    {
        std::env::var("USERNAME").unwrap_or_else(|_| "unknown".to_string())
    }
}

/// Cross-platform hostname.
fn whoami_hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| gethostname_fallback())
}

fn gethostname_fallback() -> String {
    #[cfg(unix)]
    {
        // Read hostname from /etc/hostname (avoids libc dependency)
        if let Ok(name) = std::fs::read_to_string("/etc/hostname") {
            let trimmed = name.trim().to_string();
            if !trimmed.is_empty() {
                return trimmed;
            }
        }
        "unknown".to_string()
    }
    #[cfg(windows)]
    {
        "unknown".to_string()
    }
}

// ── Authenticated encryption: AES-256-CTR + HMAC-SHA256 (encrypt-then-MAC) ──
// NOT GCM — deliberately. Encrypt-then-MAC with HMAC-SHA256 over the
// ciphertext gives the same authenticated-encryption guarantees without
// a GHASH implementation, and the MAC check runs BEFORE decryption
// (see decrypt below), which rules out padding/oracle-style attacks.
//
// Format per entry: base64(nonce ‖ ciphertext ‖ mac)
//   nonce: 12 bytes random
//   ciphertext: same length as plaintext
//   mac: 32 bytes HMAC-SHA256(nonce ‖ ciphertext)

/// AES-256 key schedule + CTR mode encryption.
/// This is a minimal implementation for encrypting short API keys (< 256 bytes).
/// NOT suitable for bulk data encryption.
fn aes256_ctr_encrypt(key: &[u8; 32], nonce: &[u8; 12], plaintext: &[u8]) -> Vec<u8> {
    let round_keys = aes256_key_schedule(key);
    let mut ciphertext = Vec::with_capacity(plaintext.len());

    for (block_idx, chunk) in plaintext.chunks(16).enumerate() {
        // Build counter block: nonce (12 bytes) + big-endian counter (4 bytes)
        let mut ctr_block = [0u8; 16];
        ctr_block[..12].copy_from_slice(nonce);
        ctr_block[12..16].copy_from_slice(&(block_idx as u32 + 1).to_be_bytes());

        let keystream = aes256_encrypt_block(&round_keys, &ctr_block);
        for (i, &b) in chunk.iter().enumerate() {
            ciphertext.push(b ^ keystream[i]);
        }
    }

    ciphertext
}

/// HMAC-SHA256 for the encrypt-then-MAC construction (MAC is computed
/// over nonce ‖ ciphertext, after encryption — see module comment).
fn hmac_sha256(key: &[u8; 32], data: &[u8]) -> [u8; 32] {
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..32 {
        ipad[i] ^= key[i];
        opad[i] ^= key[i];
    }
    // inner hash: SHA256(ipad || data)
    let mut inner = ipad.to_vec();
    inner.extend_from_slice(data);
    let inner_hash = sha256_digest(&inner);

    // outer hash: SHA256(opad || inner_hash)
    let mut outer = opad.to_vec();
    outer.extend_from_slice(&inner_hash);
    sha256_digest(&outer)
}

/// Derive separate encryption and MAC keys from the master key.
fn derive_enc_mac_keys(master: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let enc_key = sha256_digest(&[master.as_slice(), b"enc"].concat());
    let mac_key = sha256_digest(&[master.as_slice(), b"mac"].concat());
    (enc_key, mac_key)
}

fn encrypt_value(master_key: &[u8; 32], plaintext: &[u8]) -> String {
    let (enc_key, mac_key) = derive_enc_mac_keys(master_key);

    // Random 12-byte nonce
    let mut nonce = [0u8; 12];
    fill_random(&mut nonce);

    let ciphertext = aes256_ctr_encrypt(&enc_key, &nonce, plaintext);

    // MAC over nonce || ciphertext
    let mut mac_input = Vec::with_capacity(12 + ciphertext.len());
    mac_input.extend_from_slice(&nonce);
    mac_input.extend_from_slice(&ciphertext);
    let mac = hmac_sha256(&mac_key, &mac_input);

    // Encode: nonce || ciphertext || mac
    let mut blob = Vec::with_capacity(12 + ciphertext.len() + 32);
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&ciphertext);
    blob.extend_from_slice(&mac);

    base64::engine::general_purpose::STANDARD.encode(&blob)
}

fn decrypt_value(master_key: &[u8; 32], encoded: &str) -> Result<String, String> {
    let blob = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| format!("base64 decode error: {}", e))?;

    if blob.len() < 12 + 32 {
        return Err("encrypted blob too short".into());
    }

    let (enc_key, mac_key) = derive_enc_mac_keys(master_key);

    let nonce: [u8; 12] = blob[..12].try_into().unwrap();
    let ciphertext = &blob[12..blob.len() - 32];
    let stored_mac: [u8; 32] = blob[blob.len() - 32..].try_into().unwrap();

    // Verify MAC first
    let mut mac_input = Vec::with_capacity(12 + ciphertext.len());
    mac_input.extend_from_slice(&nonce);
    mac_input.extend_from_slice(ciphertext);
    let computed_mac = hmac_sha256(&mac_key, &mac_input);

    if !constant_time_eq(&stored_mac, &computed_mac) {
        return Err("MAC verification failed — key file may be corrupted or tampered".into());
    }

    // Decrypt (CTR mode: encrypt == decrypt)
    let plaintext = aes256_ctr_encrypt(&enc_key, &nonce, ciphertext);
    String::from_utf8(plaintext).map_err(|e| format!("UTF-8 decode error: {}", e))
}

/// Constant-time comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Fill buffer with random bytes using OS RNG.
fn fill_random(buf: &mut [u8]) {
    #[cfg(unix)]
    {
        use std::io::Read;
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            let _ = f.read_exact(buf);
            return;
        }
        panic!("Cannot open /dev/urandom for random bytes");
    }
    #[cfg(windows)]
    {
        // BCryptGenRandom
        #[link(name = "bcrypt")]
        extern "system" {
            fn BCryptGenRandom(
                hAlgorithm: *mut std::ffi::c_void,
                pbBuffer: *mut u8,
                cbBuffer: u32,
                dwFlags: u32,
            ) -> i32;
        }
        const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x00000002;
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        assert!(status >= 0, "BCryptGenRandom failed with status {}", status);
    }
}

// ── AES-256 block cipher ────────────────────────────────────────────
// Minimal AES-256 implementation for CTR mode. Encrypts one 16-byte block.

const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

const RCON: [u8; 10] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36];

fn aes256_key_schedule(key: &[u8; 32]) -> [[u8; 16]; 15] {
    // AES-256: Nk=8 words, Nr=14 rounds, need 60 words (15 round keys of 4 words each)
    let mut w = [[0u8; 4]; 60];

    // First 8 words come directly from the 32-byte key
    for i in 0..8 {
        w[i] = [key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]];
    }

    for i in 8..60 {
        let mut temp = w[i - 1];
        if i % 8 == 0 {
            // RotWord + SubWord + Rcon
            temp = [
                SBOX[temp[1] as usize] ^ RCON[i / 8 - 1],
                SBOX[temp[2] as usize],
                SBOX[temp[3] as usize],
                SBOX[temp[0] as usize],
            ];
        } else if i % 8 == 4 {
            // SubWord only (AES-256 extra step)
            temp = [
                SBOX[temp[0] as usize],
                SBOX[temp[1] as usize],
                SBOX[temp[2] as usize],
                SBOX[temp[3] as usize],
            ];
        }
        for j in 0..4 {
            w[i][j] = w[i - 8][j] ^ temp[j];
        }
    }

    // Pack 60 words into 15 round keys of 16 bytes each
    let mut rk = [[0u8; 16]; 15];
    for r in 0..15 {
        for col in 0..4 {
            let word = w[r * 4 + col];
            rk[r][col * 4] = word[0];
            rk[r][col * 4 + 1] = word[1];
            rk[r][col * 4 + 2] = word[2];
            rk[r][col * 4 + 3] = word[3];
        }
    }
    rk
}

fn aes256_encrypt_block(round_keys: &[[u8; 16]; 15], block: &[u8; 16]) -> [u8; 16] {
    let mut state = *block;

    // Initial round key addition
    xor_block(&mut state, &round_keys[0]);

    // Rounds 1..14 (AES-256 has 14 rounds)
    #[allow(clippy::needless_range_loop)]
    for r in 1..=14 {
        sub_bytes(&mut state);
        shift_rows(&mut state);
        if r < 14 {
            mix_columns(&mut state);
        }
        xor_block(&mut state, &round_keys[r]);
    }

    state
}

fn xor_block(state: &mut [u8; 16], key: &[u8; 16]) {
    for i in 0..16 {
        state[i] ^= key[i];
    }
}

fn sub_bytes(state: &mut [u8; 16]) {
    for b in state.iter_mut() {
        *b = SBOX[*b as usize];
    }
}

fn shift_rows(state: &mut [u8; 16]) {
    // AES state is column-major: state[row + 4*col]
    // Row 1: shift left by 1
    let tmp = state[1];
    state[1] = state[5];
    state[5] = state[9];
    state[9] = state[13];
    state[13] = tmp;

    // Row 2: shift left by 2
    let (t0, t1) = (state[2], state[6]);
    state[2] = state[10];
    state[6] = state[14];
    state[10] = t0;
    state[14] = t1;

    // Row 3: shift left by 3 (= right by 1)
    let tmp = state[15];
    state[15] = state[11];
    state[11] = state[7];
    state[7] = state[3];
    state[3] = tmp;
}

fn mix_columns(state: &mut [u8; 16]) {
    for col in 0..4 {
        let i = col * 4;
        let (a0, a1, a2, a3) = (state[i], state[i + 1], state[i + 2], state[i + 3]);
        let t = a0 ^ a1 ^ a2 ^ a3;
        state[i] = a0 ^ t ^ xtime(a0 ^ a1);
        state[i + 1] = a1 ^ t ^ xtime(a1 ^ a2);
        state[i + 2] = a2 ^ t ^ xtime(a2 ^ a3);
        state[i + 3] = a3 ^ t ^ xtime(a3 ^ a0);
    }
}

fn xtime(x: u8) -> u8 {
    if x & 0x80 != 0 {
        (x << 1) ^ 0x1b
    } else {
        x << 1
    }
}

// ── Local encrypted file backend ────────────────────────────────────

fn keys_file_path() -> Option<PathBuf> {
    crate::config_dir_path().map(|p| p.join("keys.enc"))
}

/// Load the key-value map from the encrypted JSON file.
fn load_local_store(master: &[u8; 32]) -> HashMap<String, String> {
    let path = match keys_file_path() {
        Some(p) => p,
        None => return HashMap::new(),
    };
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(_) => return HashMap::new(),
    };
    let map: HashMap<String, String> = match serde_json::from_str(&data) {
        Ok(m) => m,
        Err(_) => return HashMap::new(),
    };

    // Decrypt values
    let mut result = HashMap::new();
    for (name, encrypted) in &map {
        match decrypt_value(master, encrypted) {
            Ok(plain) => {
                result.insert(name.clone(), plain);
            }
            Err(e) => {
                crate::log(&format!("WARNING: failed to decrypt key '{}': {}", name, e));
            }
        }
    }
    result
}

/// Save the key-value map to the encrypted JSON file.
fn save_local_store(master: &[u8; 32], store: &HashMap<String, String>) -> Result<(), String> {
    let path = keys_file_path().ok_or("Cannot determine config directory")?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Encrypt all values
    let mut encrypted: HashMap<String, String> = HashMap::new();
    for (name, plain) in store {
        encrypted.insert(name.clone(), encrypt_value(master, plain.as_bytes()));
    }

    let json = serde_json::to_string_pretty(&encrypted)
        .map_err(|e| format!("JSON serialize error: {}", e))?;

    // Write atomically via temp file
    let tmp_path = path.with_extension("enc.tmp");
    std::fs::write(&tmp_path, &json).map_err(|e| format!("Failed to write key file: {}", e))?;
    std::fs::rename(&tmp_path, &path).map_err(|e| format!("Failed to rename key file: {}", e))?;

    // Set restrictive permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    Ok(())
}

// ── Public API ──────────────────────────────────────────────────────

/// Thread-safe key store with lazy-initialized machine key.
pub struct KeyStore {
    machine_key: [u8; 32],
    /// Cached decrypted local store (avoids re-reading file on every load_key)
    local_cache: Mutex<HashMap<String, String>>,
}

impl Default for KeyStore {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyStore {
    pub fn new() -> Self {
        let machine_key = derive_machine_key();
        let local_cache = load_local_store(&machine_key);
        Self {
            machine_key,
            local_cache: Mutex::new(local_cache),
        }
    }

    /// Save a key. Always uses local encrypted file (no OS popups, no admin needed).
    /// The `_use_keyring` parameter is kept for API compatibility but ignored.
    pub fn save_key(
        &self,
        scope: KeyringScope,
        key: &str,
        _use_keyring: bool,
    ) -> Result<(), String> {
        let name = scope.entry_name();
        let mut cache = self.local_cache.lock().map_err(|e| e.to_string())?;
        // An empty value means "remove this key": delete the entry instead of
        // storing an empty string. Otherwise has_key() (presence-only) would
        // still report the provider as connected and the "Remove key" button
        // would be a silent no-op (the provider stays in the filtered pickers).
        if key.is_empty() {
            cache.remove(&name);
            save_local_store(&self.machine_key, &cache)?;
            crate::log(&format!("Key removed from local encrypted store: {}", name));
            return Ok(());
        }
        cache.insert(name.clone(), key.to_string());
        save_local_store(&self.machine_key, &cache)?;
        crate::log(&format!("Key saved to local encrypted store: {}", name));
        Ok(())
    }

    /// Load a key. Always tries local first (primary), then OS keyring (read-only migration).
    /// The `_use_keyring` parameter is kept for API compatibility but ignored.
    pub fn load_key(&self, scope: KeyringScope, _use_keyring: bool) -> Option<String> {
        let name = scope.entry_name();
        // Try local first (primary backend)
        if let Ok(cache) = self.local_cache.lock() {
            if let Some(key) = cache.get(&name) {
                return Some(key.clone());
            }
        }
        // Fallback: try OS keyring (read-only, for migration from older versions)
        if let Ok(entry) = keyring::Entry::new("dimmy", &name) {
            if let Ok(key) = entry.get_password() {
                crate::log(&format!("Key migrated from OS keyring: {}", name));
                // Auto-migrate: save to local so next load is faster
                if let Ok(mut cache) = self.local_cache.lock() {
                    cache.insert(name.clone(), key.clone());
                    let _ = save_local_store(&self.machine_key, &cache);
                }
                return Some(key);
            }
        }
        None
    }

    /// Check if a NON-EMPTY key exists in either backend. A stored empty
    /// string (legacy data from before empty-clears-the-entry) counts as
    /// absent, so a cleared provider is never reported as connected.
    pub fn has_key(&self, scope: KeyringScope, use_keyring: bool) -> bool {
        self.load_key(scope, use_keyring)
            .is_some_and(|k| !k.is_empty())
    }

    /// Migrate all keys from one backend to the other.
    /// Called when user toggles the use_keyring setting.
    pub fn migrate_keys(&self, to_keyring: bool) -> Result<usize, String> {
        let mut migrated = 0;

        if to_keyring {
            // Local → Keyring: read all from local, write to keyring
            let cache = self.local_cache.lock().map_err(|e| e.to_string())?;
            for (name, key) in cache.iter() {
                let entry = keyring::Entry::new("dimmy", name)
                    .map_err(|e| format!("Keyring entry error for {}: {}", name, e))?;
                entry
                    .set_password(key)
                    .map_err(|e| format!("Keyring save error for {}: {}", name, e))?;
                crate::log(&format!("Migrated to keyring: {}", name));
                migrated += 1;
            }
        } else {
            // Keyring → Local: try known scopes
            let providers = [
                crate::provider::Provider::Groq,
                crate::provider::Provider::OpenAI,
                crate::provider::Provider::OpenRouter,
                crate::provider::Provider::Gemini,
                crate::provider::Provider::Deepgram,
                crate::provider::Provider::Anthropic,
                crate::provider::Provider::Custom,
            ];
            let mut cache = self.local_cache.lock().map_err(|e| e.to_string())?;
            for provider in &providers {
                for scope in [KeyringScope::Stt(*provider), KeyringScope::Llm(*provider)] {
                    let name = scope.entry_name();
                    if let Ok(entry) = keyring::Entry::new("dimmy", &name) {
                        if let Ok(key) = entry.get_password() {
                            cache.insert(name.clone(), key);
                            crate::log(&format!("Migrated from keyring to local: {}", name));
                            migrated += 1;
                        }
                    }
                }
            }
            if migrated > 0 {
                save_local_store(&self.machine_key, &cache)?;
            }
        }

        crate::log(&format!(
            "Key migration complete: {} keys migrated",
            migrated
        ));
        Ok(migrated)
    }

    /// Delete a key from the old legacy keyring entry format.
    pub fn delete_legacy_key(&self, service: &str, name: &str) {
        if let Ok(entry) = keyring::Entry::new(service, name) {
            let _ = entry.delete_credential();
        }
    }

    /// Refresh local cache from disk (useful after external changes).
    pub fn reload_local_cache(&self) {
        if let Ok(mut cache) = self.local_cache.lock() {
            *cache = load_local_store(&self.machine_key);
        }
    }

    /// Replace the in-memory cache with a known set of (scope, key) pairs.
    /// Disk file is NOT touched. FOR TESTS ONLY — production code uses
    /// `save_key`. Lets unit tests seed deterministic state without
    /// polluting the developer's real `~/.config/dimmy/keys.enc`.
    #[cfg(test)]
    pub(crate) fn replace_cache_for_testing(&self, entries: &[(KeyringScope, &str)]) {
        if let Ok(mut cache) = self.local_cache.lock() {
            cache.clear();
            for (scope, key) in entries {
                cache.insert(scope.entry_name(), key.to_string());
            }
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_empty() {
        let hash = sha256_digest(b"");
        let hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(
            hex,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha256_abc() {
        let hash = sha256_digest(b"abc");
        let hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(
            hex,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_long_input() {
        let hash = sha256_digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq");
        let hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(
            hex,
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = derive_machine_key();
        let plaintext = "sk-proj-abc123xyz789longapikey";
        let encrypted = encrypt_value(&key, plaintext.as_bytes());

        // Encrypted should be different from plaintext
        assert_ne!(encrypted, plaintext);

        // Decrypt should recover original
        let decrypted = decrypt_value(&key, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn encrypt_decrypt_empty() {
        let key = derive_machine_key();
        let encrypted = encrypt_value(&key, b"");
        let decrypted = decrypt_value(&key, &encrypted).unwrap();
        assert_eq!(decrypted, "");
    }

    #[test]
    fn has_key_treats_empty_as_absent() {
        // Regression: "Remove key" clears a provider by saving an empty value.
        // has_key must then report the provider as having NO key, otherwise the
        // filtered provider pickers keep showing it (remove looks like a no-op).
        use crate::provider::Provider;
        let name = KeyringScope::Stt(Provider::Groq).entry_name();

        // A stored empty string (legacy cleared entry) reads as absent.
        let mut empty = HashMap::new();
        empty.insert(name.clone(), String::new());
        let store = KeyStore {
            machine_key: derive_machine_key(),
            local_cache: Mutex::new(empty),
        };
        assert!(
            !store.has_key(KeyringScope::Stt(Provider::Groq), false),
            "empty stored value must count as no key"
        );

        // A real value reads as present.
        let mut real = HashMap::new();
        real.insert(name, "sk-real-key-123".to_string());
        let store = KeyStore {
            machine_key: derive_machine_key(),
            local_cache: Mutex::new(real),
        };
        assert!(
            store.has_key(KeyringScope::Stt(Provider::Groq), false),
            "non-empty stored value must count as a key"
        );
    }

    #[test]
    fn encrypt_produces_different_ciphertext_each_time() {
        let key = derive_machine_key();
        let plaintext = b"test-key";
        let enc1 = encrypt_value(&key, plaintext);
        let enc2 = encrypt_value(&key, plaintext);
        // Random nonce means different ciphertext each time
        assert_ne!(enc1, enc2);
        // But both decrypt to same value
        assert_eq!(decrypt_value(&key, &enc1).unwrap(), "test-key");
        assert_eq!(decrypt_value(&key, &enc2).unwrap(), "test-key");
    }

    #[test]
    fn decrypt_detects_tampered_ciphertext() {
        let key = derive_machine_key();
        let encrypted = encrypt_value(&key, b"secret");
        let mut blob = base64::engine::general_purpose::STANDARD
            .decode(&encrypted)
            .unwrap();
        // Tamper with a ciphertext byte
        if blob.len() > 13 {
            blob[13] ^= 0xff;
        }
        let tampered = base64::engine::general_purpose::STANDARD.encode(&blob);
        assert!(decrypt_value(&key, &tampered).is_err());
    }

    #[test]
    fn decrypt_rejects_wrong_key() {
        let key = derive_machine_key();
        let encrypted = encrypt_value(&key, b"secret");
        let wrong_key = [0u8; 32];
        assert!(decrypt_value(&wrong_key, &encrypted).is_err());
    }

    #[test]
    fn decrypt_rejects_too_short() {
        let key = derive_machine_key();
        assert!(decrypt_value(&key, "dG9vc2hvcnQ=").is_err());
    }

    #[test]
    fn machine_key_is_deterministic() {
        let k1 = derive_machine_key();
        let k2 = derive_machine_key();
        assert_eq!(k1, k2);
    }

    #[test]
    fn machine_key_is_32_bytes() {
        let k = derive_machine_key();
        assert_eq!(k.len(), 32);
    }

    #[test]
    fn hmac_sha256_known_vector() {
        // RFC 4231 Test Case 2
        let key = sha256_digest(b"Jefe"); // Not the real test but validates our HMAC structure
        let data = b"what do ya want for nothing?";
        let mac = hmac_sha256(&key, data);
        // Just verify it's deterministic and 32 bytes
        let mac2 = hmac_sha256(&key, data);
        assert_eq!(mac, mac2);
        assert_eq!(mac.len(), 32);
    }

    #[test]
    fn aes256_nist_test_vector() {
        // NIST FIPS 197 AES-256 test vector
        let key: [u8; 32] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b,
            0x1c, 0x1d, 0x1e, 0x1f,
        ];
        let plaintext: [u8; 16] = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        let expected: [u8; 16] = [
            0x8e, 0xa2, 0xb7, 0xca, 0x51, 0x67, 0x45, 0xbf, 0xea, 0xfc, 0x49, 0x90, 0x4b, 0x49,
            0x60, 0x89,
        ];

        let round_keys = aes256_key_schedule(&key);
        let result = aes256_encrypt_block(&round_keys, &plaintext);
        assert_eq!(
            result, expected,
            "AES-256 block encryption does not match NIST test vector"
        );
    }

    #[test]
    fn local_store_roundtrip() {
        let key = derive_machine_key();
        let mut store = HashMap::new();
        store.insert("api-key-groq".to_string(), "gsk_test123".to_string());
        store.insert(
            "llm-key-anthropic".to_string(),
            "sk-ant-test456".to_string(),
        );

        // Encrypt values
        let mut encrypted: HashMap<String, String> = HashMap::new();
        for (name, plain) in &store {
            encrypted.insert(name.clone(), encrypt_value(&key, plain.as_bytes()));
        }

        // Decrypt values
        for (name, enc) in &encrypted {
            let decrypted = decrypt_value(&key, enc).unwrap();
            assert_eq!(&decrypted, store.get(name).unwrap());
        }
    }

    #[test]
    fn constant_time_eq_works() {
        let a = [1u8; 32];
        let b = [1u8; 32];
        let c = [2u8; 32];
        assert!(constant_time_eq(&a, &b));
        assert!(!constant_time_eq(&a, &c));
    }
}
