use ring::signature::{ED25519, UnparsedPublicKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

const SIGNATURE_SCHEMA: &str = "medusa-rolling-signature-v1";
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_SIGNATURE_BYTES: usize = 16 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignatureEnvelope {
    schema: String,
    key_id: String,
    algorithm: String,
    manifest_sha256: String,
    signature: String,
}

#[derive(Debug, Deserialize)]
struct Keyring {
    keys: Vec<KeyringEntry>,
}

#[derive(Debug, Deserialize)]
struct KeyringEntry {
    key_id: String,
    public_key_hex: String,
    status: String,
}

pub(crate) fn verify(manifest_bytes: &[u8], signature_bytes: &[u8]) -> MedusaResult<()> {
    if manifest_bytes.is_empty() || manifest_bytes.len() > MAX_MANIFEST_BYTES {
        return Err(invalid("rolling manifest exceeds its allowed size"));
    }
    if signature_bytes.is_empty() || signature_bytes.len() > MAX_SIGNATURE_BYTES {
        return Err(invalid("rolling signature exceeds its allowed size"));
    }
    let envelope: SignatureEnvelope = serde_json::from_slice(signature_bytes)
        .map_err(|error| invalid(format!("invalid rolling signature envelope: {error}")))?;
    if envelope.schema != SIGNATURE_SCHEMA || envelope.algorithm != "Ed25519" {
        return Err(invalid("unsupported rolling signature envelope"));
    }
    let keyring: Keyring = serde_json::from_str(include_str!("../../../release/keys/keyring.json"))
        .map_err(|error| invalid(format!("invalid embedded release keyring: {error}")))?;
    let key = keyring
        .keys
        .iter()
        .find(|entry| entry.key_id == envelope.key_id)
        .ok_or_else(|| invalid(format!("unknown rolling signing key {}", envelope.key_id)))?;
    if key.status != "active" {
        return Err(invalid(format!(
            "rolling signing key {} is not active",
            envelope.key_id
        )));
    }
    let digest = hex::encode(Sha256::digest(manifest_bytes));
    if envelope.manifest_sha256 != digest {
        return Err(invalid("rolling manifest digest does not match its signature envelope"));
    }
    let public_key = hex::decode(&key.public_key_hex)
        .map_err(|_| invalid("rolling signing key has invalid hex"))?;
    if public_key.len() != 32 {
        return Err(invalid("rolling signing key has invalid length"));
    }
    let signature = hex::decode(&envelope.signature)
        .map_err(|_| invalid("rolling signature is not valid hex"))?;
    if signature.len() != 64 {
        return Err(invalid("rolling signature has invalid length"));
    }
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(manifest_bytes, &signature)
        .map_err(|_| invalid("rolling manifest signature is invalid"))
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

#[cfg(test)]
mod tests {
    use ring::signature::{Ed25519KeyPair, KeyPair};

    use super::*;

    #[test]
    fn rejects_unsigned_or_tampered_rolling_authority() {
        let manifest = br#"{"schema":"medusa-main-artifact-v1"}"#;
        assert!(verify(manifest, b"{}").is_err());

        let key_pair = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).expect("test key");
        let envelope = SignatureEnvelope {
            schema: SIGNATURE_SCHEMA.to_owned(),
            key_id: "test-key".to_owned(),
            algorithm: "Ed25519".to_owned(),
            manifest_sha256: hex::encode(Sha256::digest(manifest)),
            signature: hex::encode(key_pair.sign(manifest).as_ref()),
        };
        assert_eq!(key_pair.public_key().as_ref().len(), 32);
        let encoded = serde_json::to_vec(&serde_json::json!({
            "schema": envelope.schema,
            "key_id": envelope.key_id,
            "algorithm": envelope.algorithm,
            "manifest_sha256": envelope.manifest_sha256,
            "signature": envelope.signature,
        }))
        .expect("signature");
        // The test key is intentionally absent from the production keyring.
        assert!(verify(manifest, &encoded).is_err());
    }
}
