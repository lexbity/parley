//! HMAC-SHA256 (RFC 2104) with domain separation for command ledger keys.
//!
//! Key digests for receipts and cursors are HMACs under a private generation
//! key. Domain separation prevents a digest computed for one purpose (for
//! example a receipt key) from being replayed for another (a cursor). The
//! implementation is pure; it is qualified against RFC 4231 vectors.

use sha2::{Digest, Sha256};

const BLOCK: usize = 64;
const HASH_LEN: usize = 32;

/// HMAC-SHA256 over `data` with domain-separated prefix `domain`.
pub fn sign(key: &[u8], domain: &[u8], data: &[u8]) -> [u8; HASH_LEN] {
    let mut key_block = [0u8; BLOCK];
    if key.len() > BLOCK {
        let digest = Sha256::digest(key);
        key_block[..HASH_LEN].copy_from_slice(&digest);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for (byte, key_byte) in ipad.iter_mut().zip(key_block.iter()) {
        *byte ^= key_byte;
    }
    for (byte, key_byte) in opad.iter_mut().zip(key_block.iter()) {
        *byte ^= key_byte;
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(domain);
    inner.update(data);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_digest);
    let mut out = [0u8; HASH_LEN];
    out.copy_from_slice(&outer.finalize());
    out
}

/// Hex-encoded HMAC-SHA256 with domain separation.
pub fn sign_hex(key: &[u8], domain: &[u8], data: &[u8]) -> String {
    hex::encode(sign(key, domain, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4231_test_case_1() {
        let key = [0x0b; 20];
        let digest = sign(&key, b"", b"Hi There");
        assert_eq!(
            hex::encode(digest),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn rfc4231_test_case_2() {
        let digest = sign(b"Jefe", b"", b"what do ya want for nothing?");
        assert_eq!(
            hex::encode(digest),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn rfc4231_test_case_3() {
        let key = [0xaa; 20];
        let data = [0xdd; 50];
        let digest = sign(&key, b"", &data);
        assert_eq!(
            hex::encode(digest),
            "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"
        );
    }

    #[test]
    fn rfc4231_test_case_6() {
        let key = vec![0xaau8; 131];
        let data = b"Test Using Larger Than Block-Size Key - Hash Key First";
        let digest = sign(&key, b"", data);
        assert_eq!(
            hex::encode(digest),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn domain_separation_changes_digest() {
        let key = b"generation-key";
        let a = sign_hex(key, b"receipt", b"same-data");
        let b = sign_hex(key, b"cursor", b"same-data");
        assert_ne!(a, b);
        // Same domain and data is stable.
        assert_eq!(sign_hex(key, b"receipt", b"same-data"), a);
    }

    #[test]
    fn long_keys_are_hashed_first() {
        let long = vec![0x42u8; 200];
        let a = sign_hex(&long, b"d", b"x");
        let b = sign_hex(&long, b"d", b"x");
        assert_eq!(a, b);
    }
}
