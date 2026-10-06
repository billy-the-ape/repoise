//! Content and manifest hashing (SHA-256, hex-encoded).

use sha2::{Digest, Sha256};

/// Returns the lowercase SHA-256 hex digest of `bytes`.
pub fn sha256_hex(bytes: impl AsRef<[u8]>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes.as_ref());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
