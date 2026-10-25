use ed25519_dalek::{Signature, Verifier, VerifyingKey};
#[cfg(test)]
use ed25519_dalek::{Signer, SigningKey};

fn decode_32(value: &str, what: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(value.trim()).map_err(|_| format!("{what} is not valid hex"))?;
    bytes
        .try_into()
        .map_err(|_| format!("{what} must be 32 bytes"))
}

/// Trusted key specifications are `id:HEX` or a bare `HEX` (id `default`).
pub fn parse_key_spec(spec: &str) -> Result<(String, VerifyingKey), String> {
    let (id, hex_value) = match spec.split_once(':') {
        Some((id, value)) => (id.trim().to_string(), value.trim()),
        None => ("default".to_string(), spec.trim()),
    };
    if id.is_empty() {
        return Err("trusted key id cannot be empty".into());
    }
    let bytes = decode_32(hex_value, "public key")?;
    let key = VerifyingKey::from_bytes(&bytes).map_err(|e| format!("invalid public key: {e}"))?;
    Ok((id, key))
}

pub fn verify_signature(
    trusted_keys: &[String],
    key_id: &str,
    message: &[u8],
    signature_hex: &str,
) -> Result<(), String> {
    if trusted_keys.is_empty() {
        return Err("no trusted keys configured".into());
    }
    let signature_bytes =
        hex::decode(signature_hex.trim()).map_err(|_| "signature is not valid hex".to_string())?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|_| "signature must be 64 bytes".to_string())?;
    let wanted = if key_id.trim().is_empty() {
        "default"
    } else {
        key_id.trim()
    };
    let mut matched = false;
    for spec in trusted_keys {
        let (id, key) = parse_key_spec(spec)?;
        if id != wanted {
            continue;
        }
        matched = true;
        if key.verify(message, &signature).is_ok() {
            return Ok(());
        }
    }
    if matched {
        Err("signature verification failed".into())
    } else {
        Err(format!("no trusted key with id '{wanted}'"))
    }
}

/// Signing helpers back unit-test fixtures only. The runtime verifies signatures
/// and never holds organization signing keys; production signing belongs in KMS/HSM.
#[cfg(test)]
pub fn sign_hex(secret_hex: &str, message: &[u8]) -> Result<String, String> {
    let bytes = decode_32(secret_hex, "signing key")?;
    let key = SigningKey::from_bytes(&bytes);
    Ok(hex::encode(key.sign(message).to_bytes()))
}

#[cfg(test)]
pub fn public_key_hex(secret_hex: &str) -> Result<String, String> {
    let bytes = decode_32(secret_hex, "signing key")?;
    let key = SigningKey::from_bytes(&bytes);
    Ok(hex::encode(key.verifying_key().to_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_round_trip_and_reject_wrong_keys() {
        let secret = "11".repeat(32);
        let public = public_key_hex(&secret).unwrap();
        let message = b"canonical policy bytes";
        let signature = sign_hex(&secret, message).unwrap();
        assert!(verify_signature(
            &[format!("default:{public}")],
            "default",
            message,
            &signature
        )
        .is_ok());
        assert!(verify_signature(
            &[format!("default:{public}")],
            "default",
            b"different bytes",
            &signature
        )
        .is_err());
        let other = public_key_hex(&"22".repeat(32)).unwrap();
        assert!(verify_signature(
            &[format!("default:{other}")],
            "default",
            message,
            &signature
        )
        .is_err());
    }
}
