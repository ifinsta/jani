use ed25519_dalek::{Signature, VerifyingKey};
#[cfg(any(test, feature = "test-keys"))]
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

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
    if key.is_weak() || key.to_edwards().compress().to_bytes() != bytes {
        return Err("public key must be a canonical Ed25519 key of non-small order".into());
    }
    Ok((id, key))
}

pub fn verify_signature(
    trusted_keys: &[String],
    key_id: &str,
    message: &[u8],
    signature_hex: &str,
) -> Result<(), String> {
    verify_signature_with_revoked(trusted_keys, &[], key_id, message, signature_hex)
}

pub fn verify_signature_with_revoked(
    trusted_keys: &[String],
    revoked_key_ids: &[String],
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
    if revoked_key_ids
        .iter()
        .any(|revoked| revoked.trim() == wanted)
    {
        return Err(format!("signing key '{wanted}' has been revoked"));
    }
    let mut keys = std::collections::BTreeMap::new();
    for spec in trusted_keys {
        let (id, key) = parse_key_spec(spec)?;
        if keys
            .insert(id.clone(), key)
            .is_some_and(|previous| previous != key)
        {
            return Err(format!("trusted key id '{id}' has conflicting public keys"));
        }
    }
    let key = keys
        .get(wanted)
        .ok_or_else(|| format!("no trusted key with id '{wanted}'"))?;
    if revoked_key_ids
        .iter()
        .any(|id| keys.get(id.trim()) == Some(key))
    {
        return Err(format!(
            "signing key '{wanted}' has been revoked under another trusted key id"
        ));
    }
    for revoked in revoked_key_ids
        .iter()
        .filter_map(|entry| entry.trim().strip_prefix("sha256:"))
    {
        let fingerprint = decode_32(revoked, "revoked key fingerprint")?;
        if <[u8; 32]>::from(Sha256::digest(key.as_bytes())) == fingerprint {
            return Err(format!(
                "signing key '{wanted}' has been revoked by public key fingerprint"
            ));
        }
    }
    key.verify_strict(message, &signature)
        .map_err(|_| "signature verification failed".into())
}

/// Expand known revoked IDs into deny-only fingerprints without merging trust domains.
pub(crate) fn revoked_fingerprints<'a>(
    revoked: &[String],
    roots: impl Iterator<Item = &'a String>,
) -> Result<Vec<String>, String> {
    let ids: std::collections::BTreeSet<&str> = revoked.iter().map(|value| value.trim()).collect();
    for fingerprint in ids.iter().filter_map(|entry| entry.strip_prefix("sha256:")) {
        decode_32(fingerprint, "revoked key fingerprint")?;
    }
    let mut fingerprints = std::collections::BTreeSet::new();
    for spec in roots {
        let id = spec.split_once(':').map_or("default", |(id, _)| id.trim());
        if ids.contains(id) {
            let (_, key) = parse_key_spec(spec)?;
            let fingerprint = format!("sha256:{}", hex::encode(Sha256::digest(key.as_bytes())));
            if !ids.contains(fingerprint.as_str()) {
                fingerprints.insert(fingerprint);
            }
        }
    }
    Ok(fingerprints.into_iter().collect())
}

/// Serialize a JSON value following RFC 8785 (JSON Canonicalization Scheme):
/// finite numbers in ECMAScript representation, object members sorted by
/// UTF-16 code unit order, and compact formatting with no insignificant whitespace.
pub fn canonical_json(value: &serde_json::Value) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    write_jcs(value, &mut out)?;
    Ok(out)
}

pub fn canonical_serialize<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, String> {
    let json = serde_json::to_value(value).map_err(|e| e.to_string())?;
    canonical_json(&json)
}

fn write_jcs(value: &serde_json::Value, out: &mut Vec<u8>) -> Result<(), String> {
    use serde_json::Value;
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Number(num) => write_jcs_number(num, out)?,
        Value::String(text) => {
            let encoded = serde_json::to_vec(text).map_err(|e| e.to_string())?;
            out.extend_from_slice(&encoded);
        }
        Value::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_jcs(item, out)?;
            }
            out.push(b']');
        }
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
            out.push(b'{');
            for (index, (key, val)) in entries.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                let encoded_key = serde_json::to_vec(key).map_err(|e| e.to_string())?;
                out.extend_from_slice(&encoded_key);
                out.push(b':');
                write_jcs(val, out)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}

fn write_jcs_number(num: &serde_json::Number, out: &mut Vec<u8>) -> Result<(), String> {
    if let Some(i) = num.as_i64() {
        out.extend_from_slice(i.to_string().as_bytes());
        return Ok(());
    }
    if let Some(u) = num.as_u64() {
        out.extend_from_slice(u.to_string().as_bytes());
        return Ok(());
    }
    let f = num
        .as_f64()
        .filter(|v| v.is_finite())
        .ok_or_else(|| "canonical JSON rejects non-finite numbers".to_string())?;
    if f == 0.0 {
        out.push(b'0');
        return Ok(());
    }
    // ES6 Number::toString / RFC 8785 §3.2.2.3:
    // Use serde_json's shortest round-trip representation, then normalize
    // integer `.0` and scientific notation boundaries to match ECMAScript.
    let raw = serde_json::to_string(&f).map_err(|e| e.to_string())?;
    let abs = f.abs();
    if let Some(stripped) = raw.strip_suffix(".0") {
        if abs < 1e21 {
            out.extend_from_slice(stripped.as_bytes());
            return Ok(());
        }
    }
    if !(1e-6..1e21).contains(&abs) {
        // Normalize ryu output into ECMAScript exponential form (e.g. `1e+21`, `1e-7`).
        let negative = f.is_sign_negative();
        let abs_str = if negative { &raw[1..] } else { &raw[..] };
        let formatted = to_ecmascript_exp(abs_str);
        if negative {
            out.push(b'-');
        }
        out.extend_from_slice(formatted.as_bytes());
    } else if let Some((mantissa, exp)) = raw.split_once(['e', 'E']) {
        // Numbers in [1e-6, 1e21) must be printed in fixed notation without `e`.
        let negative = mantissa.starts_with('-');
        let digits_part = if negative { &mantissa[1..] } else { mantissa };
        let exp: i32 = exp
            .parse()
            .map_err(|_| "invalid float exponent".to_string())?;
        let fixed = exp_to_fixed(digits_part, exp);
        if negative {
            out.push(b'-');
        }
        out.extend_from_slice(fixed.as_bytes());
    } else {
        out.extend_from_slice(raw.as_bytes());
    }
    Ok(())
}

fn split_significant_digits(abs_str: &str) -> (String, i32) {
    if let Some((mantissa, exp_str)) = abs_str.split_once(['e', 'E']) {
        let exp: i32 = exp_str.parse().unwrap_or(0);
        let (digits, shift) = strip_decimal(mantissa);
        (digits, exp + shift)
    } else {
        strip_decimal(abs_str)
    }
}

fn strip_decimal(s: &str) -> (String, i32) {
    let mut digits = String::with_capacity(s.len());
    let mut dot_pos: Option<usize> = None;
    for ch in s.chars() {
        if ch == '.' {
            dot_pos = Some(digits.len());
        } else {
            digits.push(ch);
        }
    }
    let trimmed_leading = digits.trim_start_matches('0');
    let leading_zeros = digits.len() - trimmed_leading.len();
    let trimmed = trimmed_leading.trim_end_matches('0').to_string();
    let int_len = dot_pos.unwrap_or(digits.len()) as i32;
    let exp = int_len - leading_zeros as i32 - 1;
    (
        if trimmed.is_empty() {
            "0".into()
        } else {
            trimmed
        },
        exp,
    )
}

fn to_ecmascript_exp(abs_str: &str) -> String {
    let (digits, exp) = split_significant_digits(abs_str);
    let sign = if exp >= 0 { "+" } else { "" };
    if digits.len() <= 1 {
        format!("{digits}e{sign}{exp}")
    } else {
        format!("{}.{}e{sign}{exp}", &digits[..1], &digits[1..])
    }
}

fn exp_to_fixed(mantissa: &str, exp: i32) -> String {
    let (digits, shift) = strip_decimal(mantissa);
    let total_exp = exp + shift;
    if total_exp < 0 {
        let zeros = (-total_exp - 1) as usize;
        format!("0.{}{}", "0".repeat(zeros), digits)
    } else {
        let split = (total_exp as usize) + 1;
        if split >= digits.len() {
            format!("{}{}", digits, "0".repeat(split - digits.len()))
        } else {
            format!("{}.{}", &digits[..split], &digits[split..])
        }
    }
}

/// Signing helpers back unit-test fixtures only. The runtime verifies signatures
/// and never holds organization signing keys; production signing belongs in KMS/HSM.
#[cfg(any(test, feature = "test-keys"))]
pub fn sign_hex(secret_hex: &str, message: &[u8]) -> Result<String, String> {
    let bytes = decode_32(secret_hex, "signing key")?;
    let key = SigningKey::from_bytes(&bytes);
    Ok(hex::encode(key.sign(message).to_bytes()))
}

#[cfg(any(test, feature = "test-keys"))]
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
        assert!(verify_signature_with_revoked(
            &[format!("default:{public}")],
            &["default".to_string()],
            "default",
            message,
            &signature
        )
        .is_err());
    }

    #[test]
    fn rotation_requires_unambiguous_key_ids() {
        let first = "11".repeat(32);
        let second = "22".repeat(32);
        let first_public = public_key_hex(&first).unwrap();
        let second_public = public_key_hex(&second).unwrap();
        let message = b"rotation fixture";
        let signature = sign_hex(&first, message).unwrap();
        let first_key = format!("current:{first_public}");
        let conflicting = format!("current:{second_public}");
        for roots in [
            [first_key.clone(), conflicting.clone()],
            [conflicting, first_key.clone()],
        ] {
            assert!(verify_signature(&roots, "current", message, &signature)
                .unwrap_err()
                .contains("conflicting"));
        }
        assert!(verify_signature(
            &[first_key, format!("next:{second_public}")],
            "current",
            message,
            &signature
        )
        .is_ok());
        assert!(verify_signature(
            &[
                first_public.clone(),
                format!(" default :{}", first_public.to_uppercase())
            ],
            "default",
            message,
            &signature
        )
        .is_ok());
    }

    #[test]
    fn revocations_follow_key_material_and_survive_removed_ids() {
        let secret = "33".repeat(32);
        let message = b"retirement fixture";
        let public = public_key_hex(&secret).unwrap();
        let roots = [format!("old:{public}"), format!("alias:{public}")];
        let revoked = ["old".to_string()];
        let signature = sign_hex(&secret, message).unwrap();
        assert!(
            verify_signature_with_revoked(&roots, &revoked, "alias", message, &signature)
                .unwrap_err()
                .contains("another trusted key id")
        );
        let fingerprints = revoked_fingerprints(&revoked, roots.iter()).unwrap();
        assert_eq!(fingerprints.len(), 1);
        assert!(verify_signature_with_revoked(
            &roots[1..],
            &fingerprints,
            "alias",
            message,
            &signature
        )
        .unwrap_err()
        .contains("fingerprint"));
        assert!(verify_signature_with_revoked(
            &roots[1..],
            &["sha256:invalid".into()],
            "alias",
            message,
            &signature
        )
        .is_err());
    }

    #[test]
    fn identity_point_is_not_an_accepted_trust_root() {
        assert!(parse_key_spec(&format!("fixture:01{}", "00".repeat(31))).is_err());
    }

    #[test]
    fn canonical_json_follows_rfc8785_jcs() {
        let input = serde_json::json!({
            "b": 1.0,
            "a": -0.0,
            "nested": { "z": true, "m": 1e25_f64 },
            "arr": [0.000001, 0.0000001]
        });
        let encoded = String::from_utf8(canonical_json(&input).unwrap()).unwrap();
        assert_eq!(
            encoded,
            r#"{"a":0,"arr":[0.000001,1e-7],"b":1,"nested":{"m":1e+25,"z":true}}"#
        );
    }
}
