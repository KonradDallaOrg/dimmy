//! Generate keys.enc for a specific machine identity (Windows).
//! This test derives the encryption key using the Windows username+hostname
//! instead of the current WSL identity, so the file can be used by Dimmy on Windows.
//!
//! Run with: cargo test --test gen_windows_keys -- --nocapture

use std::collections::HashMap;
use std::io::Write;

// We need to replicate the key derivation and encryption from keystore.rs
// but with a forced username+hostname. Since KeyStore::new() uses the current
// machine identity, we call the internal crypto functions directly.

/// SHA-256 digest (copy from keystore.rs — same pure-Rust implementation)
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
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
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

fn derive_key_for(username: &str, hostname: &str) -> [u8; 32] {
    let mut input = Vec::new();
    input.extend_from_slice(username.as_bytes());
    input.extend_from_slice(b":");
    input.extend_from_slice(hostname.as_bytes());
    input.extend_from_slice(b":dimmy-local-key-v1");
    sha256_digest(&input)
}

fn derive_enc_mac_keys(master: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let enc_key = sha256_digest(&[master.as_slice(), b"enc"].concat());
    let mac_key = sha256_digest(&[master.as_slice(), b"mac"].concat());
    (enc_key, mac_key)
}

fn hmac_sha256(key: &[u8; 32], data: &[u8]) -> [u8; 32] {
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..32 {
        ipad[i] ^= key[i];
        opad[i] ^= key[i];
    }
    let mut inner = ipad.to_vec();
    inner.extend_from_slice(data);
    let inner_hash = sha256_digest(&inner);
    let mut outer = opad.to_vec();
    outer.extend_from_slice(&inner_hash);
    sha256_digest(&outer)
}

// AES-256 (same as keystore.rs)
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
    let mut w = [[0u8; 4]; 60];
    for i in 0..8 {
        w[i] = [key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]];
    }
    for i in 8..60 {
        let mut temp = w[i - 1];
        if i % 8 == 0 {
            temp = [
                SBOX[temp[1] as usize] ^ RCON[i / 8 - 1],
                SBOX[temp[2] as usize],
                SBOX[temp[3] as usize],
                SBOX[temp[0] as usize],
            ];
        } else if i % 8 == 4 {
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
    let mut rk = [[0u8; 16]; 15];
    for r in 0..15 {
        for c in 0..4 {
            let word = w[r * 4 + c];
            rk[r][c * 4] = word[0];
            rk[r][c * 4 + 1] = word[1];
            rk[r][c * 4 + 2] = word[2];
            rk[r][c * 4 + 3] = word[3];
        }
    }
    rk
}

fn aes256_encrypt_block(rk: &[[u8; 16]; 15], block: &[u8; 16]) -> [u8; 16] {
    let mut s = *block;
    for i in 0..16 {
        s[i] ^= rk[0][i];
    }
    #[allow(clippy::needless_range_loop)]
    for r in 1..=14 {
        // SubBytes
        for b in s.iter_mut() {
            *b = SBOX[*b as usize];
        }
        // ShiftRows
        let t = s[1];
        s[1] = s[5];
        s[5] = s[9];
        s[9] = s[13];
        s[13] = t;
        let (t0, t1) = (s[2], s[6]);
        s[2] = s[10];
        s[6] = s[14];
        s[10] = t0;
        s[14] = t1;
        let t = s[15];
        s[15] = s[11];
        s[11] = s[7];
        s[7] = s[3];
        s[3] = t;
        // MixColumns (skip last round)
        if r < 14 {
            for col in 0..4 {
                let i = col * 4;
                let (a0, a1, a2, a3) = (s[i], s[i + 1], s[i + 2], s[i + 3]);
                let t = a0 ^ a1 ^ a2 ^ a3;
                let xt = |x: u8| -> u8 {
                    if x & 0x80 != 0 {
                        (x << 1) ^ 0x1b
                    } else {
                        x << 1
                    }
                };
                s[i] = a0 ^ t ^ xt(a0 ^ a1);
                s[i + 1] = a1 ^ t ^ xt(a1 ^ a2);
                s[i + 2] = a2 ^ t ^ xt(a2 ^ a3);
                s[i + 3] = a3 ^ t ^ xt(a3 ^ a0);
            }
        }
        for i in 0..16 {
            s[i] ^= rk[r][i];
        }
    }
    s
}

fn aes256_ctr(key: &[u8; 32], nonce: &[u8; 12], data: &[u8]) -> Vec<u8> {
    let rk = aes256_key_schedule(key);
    let mut out = Vec::with_capacity(data.len());
    for (idx, chunk) in data.chunks(16).enumerate() {
        let mut ctr = [0u8; 16];
        ctr[..12].copy_from_slice(nonce);
        ctr[12..16].copy_from_slice(&(idx as u32 + 1).to_be_bytes());
        let ks = aes256_encrypt_block(&rk, &ctr);
        for (i, &b) in chunk.iter().enumerate() {
            out.push(b ^ ks[i]);
        }
    }
    out
}

fn fill_random(buf: &mut [u8]) {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")
        .unwrap()
        .read_exact(buf)
        .unwrap();
}

fn encrypt_value(master: &[u8; 32], plaintext: &[u8]) -> String {
    let (enc_key, mac_key) = derive_enc_mac_keys(master);
    let mut nonce = [0u8; 12];
    fill_random(&mut nonce);
    let ct = aes256_ctr(&enc_key, &nonce, plaintext);
    let mut mac_input = Vec::with_capacity(12 + ct.len());
    mac_input.extend_from_slice(&nonce);
    mac_input.extend_from_slice(&ct);
    let mac = hmac_sha256(&mac_key, &mac_input);
    let mut blob = Vec::with_capacity(12 + ct.len() + 32);
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&ct);
    blob.extend_from_slice(&mac);
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(&blob)
}

fn load_env() -> HashMap<String, String> {
    let env_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".env");
    let mut map = HashMap::new();
    if let Ok(data) = std::fs::read_to_string(&env_path) {
        for line in data.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                map.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    map
}

#[test]
fn generate_windows_keys_enc() {
    let env = load_env();

    // Windows machine identity
    let win_username = "konradd";
    let win_hostname = "PC-KDALLA";
    let master_key = derive_key_for(win_username, win_hostname);

    println!(
        "\n=== Generating keys.enc for Windows ({} @ {}) ===\n",
        win_username, win_hostname
    );

    let entries: Vec<(&str, &str)> = vec![
        ("api-key-groq", "GROQ_KEY"),
        ("api-key-openai", "OPENAI_KEY"),
        ("api-key-gemini", "GEMINI_KEY"),
        ("api-key-deepgram", "DEEPGRAM_KEY"),
        ("llm-key-anthropic", "ANTHROPIC_KEY"),
    ];

    let mut encrypted: HashMap<String, String> = HashMap::new();
    for (name, env_var) in &entries {
        if let Some(key) = env.get(*env_var) {
            encrypted.insert(name.to_string(), encrypt_value(&master_key, key.as_bytes()));
            let preview = if key.len() > 12 {
                format!("{}...{}", &key[..6], &key[key.len() - 4..])
            } else {
                "***".to_string()
            };
            println!("  [OK] {} = {} ({} chars)", name, preview, key.len());
        } else {
            println!("  [!!] {} — {} not in .env", name, env_var);
        }
    }

    let json = serde_json::to_string_pretty(&encrypted).unwrap();
    let out_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("tests")
        .join("keys_windows.enc");
    std::fs::write(&out_path, &json).unwrap();
    println!("\n  Written to: {}", out_path.display());
    println!("  Size: {} bytes", json.len());
    println!("\n  Copy to: C:\\Users\\konradd\\AppData\\Roaming\\dimmy\\keys.enc\n");
}
