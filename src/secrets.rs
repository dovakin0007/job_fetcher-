//! Encrypted API-key storage.
//!
//! Search keys must stay usable — the app calls SerpApi/Tavily with them — so
//! one-way hashing cannot work here. Instead each key is sealed with
//! AES-256-GCM under a machine-local master key (`AGENT_MASTER_KEY`, else an
//! auto-created `data/.master_key`) with a fresh random nonce per key, and
//! only `••••last4` hints ever leave the backend or touch the UI.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};

pub struct Sealed {
    pub nonce_b64: String,
    pub ciphertext_b64: String,
    pub last4: String,
}

/// Seal a plaintext key. Trims whitespace; rejects empty input.
pub fn seal(plaintext: &str) -> anyhow::Result<Sealed> {
    let plaintext = plaintext.trim();
    if plaintext.is_empty() {
        anyhow::bail!("empty secret");
    }
    let cipher = Aes256Gcm::new_from_slice(&master_key()?).expect("32-byte master key");
    let mut nonce_bytes = [0u8; 12];
    getrandom::getrandom(&mut nonce_bytes).map_err(|e| anyhow::anyhow!("random failed: {e}"))?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_bytes())
        .map_err(|e| anyhow::anyhow!("seal failed: {e}"))?;
    Ok(Sealed {
        nonce_b64: B64.encode(nonce_bytes),
        ciphertext_b64: B64.encode(ciphertext),
        last4: last4_of(plaintext),
    })
}

/// Open a sealed key back to plaintext. Fails on tampering or a wrong key.
pub fn open(nonce_b64: &str, ciphertext_b64: &str) -> anyhow::Result<String> {
    let cipher = Aes256Gcm::new_from_slice(&master_key()?).expect("32-byte master key");
    let nonce = B64
        .decode(nonce_b64.trim())
        .map_err(|e| anyhow::anyhow!("bad nonce: {e}"))?;
    let ciphertext = B64
        .decode(ciphertext_b64.trim())
        .map_err(|e| anyhow::anyhow!("bad ciphertext: {e}"))?;
    let bytes = cipher
        .decrypt(Nonce::from_slice(&nonce), ciphertext.as_slice())
        .map_err(|_| anyhow::anyhow!("decrypt failed: wrong key or tampered data"))?;
    String::from_utf8(bytes).map_err(|e| anyhow::anyhow!("secret is not UTF-8: {e}"))
}

/// UI-safe hint for a key we only hold in memory: dots plus last four.
pub fn hint_of(key: &str) -> String {
    mask(&last4_of(key))
}

/// UI-safe hint: never the key, just dots plus the last four characters.
pub fn mask(last4: &str) -> String {
    format!("••••••••{}", last4.trim())
}

fn last4_of(key: &str) -> String {
    let chars: Vec<char> = key.trim().chars().collect();
    chars.iter().skip(chars.len().saturating_sub(4)).collect()
}

fn master_key() -> anyhow::Result<[u8; 32]> {
    // Test override first so parallel test threads never race on env vars.
    #[cfg(test)]
    if let Some(key) = test_override() {
        return Ok(key);
    }
    if let Ok(hex) = std::env::var("AGENT_MASTER_KEY") {
        return decode32(hex.trim());
    }
    let path = "data/.master_key";
    if let Ok(hex) = std::fs::read_to_string(path) {
        return decode32(hex.trim());
    }
    let mut key = [0u8; 32];
    getrandom::getrandom(&mut key).map_err(|e| anyhow::anyhow!("random failed: {e}"))?;
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, hex_encode(&key))?;
    Ok(key)
}

fn decode32(hex: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = hex_decode(hex)?;
    if bytes.len() != 32 {
        anyhow::bail!(
            "AGENT_MASTER_KEY must be 64 hex chars (32 bytes), got {}",
            bytes.len()
        );
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn hex_decode(hex: &str) -> anyhow::Result<Vec<u8>> {
    let hex = hex.trim();
    if hex.len() % 2 != 0 {
        anyhow::bail!("odd-length hex");
    }
    hex.as_bytes()
        .chunks(2)
        .map(|pair| {
            let s = std::str::from_utf8(pair).map_err(|e| anyhow::anyhow!("bad hex: {e}"))?;
            u8::from_str_radix(s, 16).map_err(|e| anyhow::anyhow!("bad hex: {e}"))
        })
        .collect()
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

#[cfg(test)]
use std::sync::OnceLock;
#[cfg(test)]
static OVERRIDE: OnceLock<std::sync::Mutex<Option<[u8; 32]>>> = OnceLock::new();

/// Test-only master-key override. All test keys use the same bytes, so
/// parallel tests can share it without racing on environment variables.
#[cfg(test)]
pub fn set_test_master_key(key: [u8; 32]) {
    *OVERRIDE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap() = Some(key);
}

#[cfg(test)]
fn test_override() -> Option<[u8; 32]> {
    OVERRIDE.get().and_then(|lock| *lock.lock().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every test uses these bytes (env or override — same value, no races).
    const TEST_BYTES: [u8; 32] = [7; 32];
    const TEST_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";

    #[test]
    fn roundtrip_and_mask() {
        set_test_master_key(TEST_BYTES);
        let sealed = seal("exa-test-key-1234").unwrap();
        assert_eq!(sealed.last4, "1234");
        assert_eq!(mask(&sealed.last4), "••••••••1234");
        assert_eq!(hint_of("exa-test-key-1234"), "••••••••1234");
        // Ciphertext must not contain the plaintext.
        assert!(!sealed.ciphertext_b64.contains("exa-test-key"));
        assert_eq!(
            open(&sealed.nonce_b64, &sealed.ciphertext_b64).unwrap(),
            "exa-test-key-1234"
        );
        // Fresh nonce per seal: same input seals differently.
        let again = seal("exa-test-key-1234").unwrap();
        assert_ne!(sealed.ciphertext_b64, again.ciphertext_b64);
    }

    #[test]
    fn tampered_ciphertext_fails() {
        set_test_master_key(TEST_BYTES);
        let mut sealed = seal("tavily-secret-9999").unwrap();
        sealed.ciphertext_b64.push('A');
        assert!(open(&sealed.nonce_b64, &sealed.ciphertext_b64).is_err());
    }

    #[test]
    fn env_key_uses_the_same_bytes() {
        // Env and override intentionally agree here, so this is race-free.
        std::env::set_var("AGENT_MASTER_KEY", TEST_HEX);
        set_test_master_key(TEST_BYTES);
        let sealed = seal("brave-key-ab12").unwrap();
        assert_eq!(
            open(&sealed.nonce_b64, &sealed.ciphertext_b64).unwrap(),
            "brave-key-ab12"
        );
        std::env::remove_var("AGENT_MASTER_KEY");
    }

    #[test]
    fn bad_hex_is_rejected() {
        assert!(decode32("short").is_err());
        assert!(decode32(&"zz".repeat(32)).is_err());
        assert!(decode32(TEST_HEX).is_ok());
    }
}
