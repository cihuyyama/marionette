//! Application-layer envelope encryption for provider credentials at rest.
//!
//! `accounts.data` holds the bearer material for every account (PATs, refresh
//! tokens, API keys). Left as-is it is readable by anyone who can open the
//! SQLite file — a backup, a copied volume, or a stolen disk is a full
//! credential dump.
//!
//! The database is therefore treated as untrusted storage: what is written is
//! ciphertext that is opaque to anything without the key. Encryption is opt-in
//! behind `MARIONETTE_DATA_KEY` so an operator who cannot manage a key is not
//! locked out of their own pool; with no key configured the column round-trips
//! exactly as before.

use aes::Aes256;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

type Aes256CbcEnc = cbc::Encryptor<Aes256>;
type Aes256CbcDec = cbc::Decryptor<Aes256>;

/// Marks a stored value as an encrypted envelope. Anything not carrying this
/// prefix is treated as legacy plaintext and read through untouched, which is
/// what makes enabling encryption a non-destructive, reversible migration.
const ENVELOPE: &str = "enc1:";

const IV_LEN: usize = 16;

/// Raw key material, resolved once from the environment.
static DATA_KEY: LazyLock<Option<Vec<u8>>> = LazyLock::new(|| {
    let raw = std::env::var("MARIONETTE_DATA_KEY").ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    // Accept either a 32-byte value or any passphrase. Hashing to 32 bytes
    // means an operator can set something memorable; a 32-byte base64/hex key
    // also lands on a full-entropy key.
    Some(Sha256::digest(raw.as_bytes()).to_vec())
});

fn data_key() -> Option<&'static [u8]> {
    DATA_KEY.as_deref()
}

/// True when a key is configured, i.e. writes will be encrypted.
pub fn enabled() -> bool {
    data_key().is_some()
}

/// Encrypt `plaintext` under `key`. `IV || ciphertext`, base64, tagged.
pub fn seal_with(key: &[u8], plaintext: &str) -> Option<String> {
    if key.len() != 32 {
        return None;
    }
    let mut iv = [0u8; IV_LEN];
    rand::thread_rng().fill_bytes(&mut iv);

    // One extra block of scratch so PKCS#7 can pad in place.
    let msg_len = plaintext.len();
    let mut buf = plaintext.as_bytes().to_vec();
    buf.resize(msg_len + 16, 0);

    let ct = Aes256CbcEnc::new_from_slices(key, &iv)
        .ok()?
        .encrypt_padded_mut::<Pkcs7>(&mut buf, msg_len)
        .ok()?
        .to_vec();

    let mut out = Vec::with_capacity(IV_LEN + ct.len());
    out.extend_from_slice(&iv);
    out.extend_from_slice(&ct);
    Some(format!("{ENVELOPE}{}", B64.encode(out)))
}

/// Decrypt a value produced by [`seal_with`]. `None` when the value is not an
/// envelope, the key is wrong, or the padding does not check out.
pub fn open_with(key: &[u8], stored: &str) -> Option<String> {
    if key.len() != 32 {
        return None;
    }
    let raw = B64.decode(stored.strip_prefix(ENVELOPE)?).ok()?;
    if raw.len() <= IV_LEN {
        return None;
    }
    let (iv, ct) = raw.split_at(IV_LEN);

    let mut buf = ct.to_vec();
    let pt = Aes256CbcDec::new_from_slices(key, iv)
        .ok()?
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .ok()?;
    String::from_utf8(pt.to_vec()).ok()
}

/// Encrypt for storage using the configured key. `None` when encryption is
/// off, in which case the caller stores the original value.
pub fn seal(plaintext: &str) -> Option<String> {
    seal_with(data_key()?, plaintext)
}

/// Decrypt a stored value using the configured key.
pub fn open(stored: &str) -> Option<String> {
    open_with(data_key()?, stored)
}

/// Read helper used wherever an account's `data` is parsed: decrypts when a
/// key is set, otherwise passes the stored value through. A DB written before
/// encryption was enabled still reads, because [`open`] rejects anything
/// without the envelope prefix.
pub fn open_or_passthrough(stored: &str) -> String {
    match open(stored) {
        Some(v) => v,
        None => stored.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> Vec<u8> {
        Sha256::digest(s.as_bytes()).to_vec()
    }

    #[test]
    fn roundtrip_under_a_key() {
        let k = key("test-key-material");
        let plain = r#"{"apiKey":"sk-abcdefgh12345678","n":1}"#;
        let sealed = seal_with(&k, plain).expect("encryption must succeed with a key");
        assert!(sealed.starts_with(ENVELOPE));
        assert!(
            !sealed.contains("sk-abcdefgh12345678"),
            "plaintext must not be visible in the stored value"
        );
        assert_eq!(open_with(&k, &sealed).as_deref(), Some(plain));
    }

    #[test]
    fn fresh_iv_per_seal() {
        let k = key("k");
        let a = seal_with(&k, "same input").unwrap();
        let b = seal_with(&k, "same input").unwrap();
        assert_ne!(a, b, "each seal must use a fresh IV");
        assert_eq!(open_with(&k, &a).unwrap(), open_with(&k, &b).unwrap());
    }

    #[test]
    fn wrong_key_fails_loudly() {
        let sealed = seal_with(&key("right"), "secret payload").unwrap();
        // A wrong key must not silently yield garbage that looks like JSON.
        assert!(
            open_with(&key("wrong"), &sealed).is_none(),
            "wrong key must fail, not return corrupt plaintext"
        );
    }

    #[test]
    fn legacy_plaintext_passes_through() {
        // A row written before encryption was enabled has no envelope.
        let legacy = r#"{"apiKey":"x","refreshToken":"y"}"#;
        assert_eq!(open_or_passthrough(legacy), legacy);
    }

    #[test]
    fn unicode_and_empty_roundtrip() {
        let k = key("k");
        for plain in ["", "héllo wörld 🌍", &"x".repeat(10_000)] {
            let sealed = seal_with(&k, plain).unwrap();
            assert_eq!(open_with(&k, &sealed).as_deref(), Some(plain));
        }
    }

    #[test]
    fn malformed_envelope_is_rejected() {
        let k = key("k");
        assert!(open_with(&k, "not-an-envelope").is_none());
        assert!(open_with(&k, &format!("{ENVELOPE}!!!!")).is_none());
        assert!(open_with(&k, &format!("{ENVELOPE}{}", B64.encode([0u8; 4]))).is_none());
        assert!(open_with(&[1u8; 32], "enc1:AAAA").is_none());
    }
}
