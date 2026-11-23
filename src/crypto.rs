//! Clés des appareils (X25519) et empreintes. Le chiffrement des échanges est dans `secure.rs`
//! (protocole Noise) : ces clés y servent à authentifier les deux appareils.

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use crypto_box::{aead::OsRng, PublicKey, SecretKey};
use sha2::{Digest, Sha256};

pub fn generate_keypair() -> SecretKey {
    SecretKey::generate(&mut OsRng)
}

pub fn encode_key(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

fn decode_key(value: &str) -> Result<[u8; 32]> {
    let bytes = STANDARD.decode(value.trim()).map_err(|_| anyhow!("clé illisible"))?;
    bytes.try_into().map_err(|_| anyhow!("clé de taille invalide"))
}

pub fn secret_from_b64(value: &str) -> Result<SecretKey> {
    Ok(SecretKey::from(decode_key(value)?))
}

pub fn public_from_b64(value: &str) -> Result<PublicKey> {
    Ok(PublicKey::from(decode_key(value)?))
}

pub fn public_to_b64(key: &PublicKey) -> String {
    encode_key(key.as_bytes())
}

/// Empreinte à comparer à l'oeil entre deux appareils : 19 caractères du SHA-256 de la clé publique.
pub fn fingerprint(public_key_b64: &str) -> String {
    let digest = Sha256::digest(public_key_b64.as_bytes());
    let encoded = STANDARD.encode(digest);
    encoded[..19].replace('/', "x")
}

pub fn sha256_b64(bytes: &[u8]) -> String {
    STANDARD.encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_and_short() {
        let fp = fingerprint("AAAA");
        assert_eq!(fp.len(), 19);
        assert_eq!(fp, fingerprint("AAAA"));
        assert_ne!(fp, fingerprint("AAAB"));
    }

    #[test]
    fn keys_survive_base64() {
        let secret = generate_keypair();
        let again = secret_from_b64(&encode_key(&secret.to_bytes())).unwrap();
        assert_eq!(again.public_key(), secret.public_key());
    }
}
