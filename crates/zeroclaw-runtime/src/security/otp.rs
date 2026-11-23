use crate::security::secrets::SecretStore;
use anyhow::{Context, Result};
use parking_lot::Mutex;
use ring::hmac;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zeroclaw_config::schema::OtpConfig;

const OTP_SECRET_FILE: &str = "otp-secret";
const OTP_DIGITS: u32 = 6;
const OTP_ISSUER: &str = "ZeroClaw";

#[derive(Debug)]
pub struct OtpValidator {
    config: OtpConfig,
    secret: Vec<u8>,
    cached_codes: Mutex<HashMap<String, u64>>,
    /// Codes already consumed by [`Self::validate_single_use`], with the time
    /// after which the same code can no longer validate anyway.
    consumed_codes: Mutex<HashMap<String, u64>>,
    /// Consecutive failed single-use attempts and when the lockout ends.
    single_use_failures: Mutex<SingleUseFailures>,
}

#[derive(Debug, Default)]
struct SingleUseFailures {
    count: u32,
    locked_until: u64,
}

/// How long single-use verification stays locked after
/// `challenge_max_attempts` consecutive failures.
const SINGLE_USE_LOCKOUT_SECS: u64 = 300;

impl OtpValidator {
    /// Load the enrolled secret without ever creating one. Unlike
    /// [`Self::from_config`] this fails when no secret file exists, so a
    /// caller that only verifies codes cannot mint a seed nobody holds.
    pub fn from_existing(
        config: &OtpConfig,
        zeroclaw_dir: &Path,
        store: &SecretStore,
    ) -> Result<Self> {
        let secret_path = secret_file_path(zeroclaw_dir);
        if !secret_path.exists() {
            anyhow::bail!(
                "No OTP secret is enrolled at {}; enable security.otp and run zeroclaw once to enroll one",
                secret_path.display()
            );
        }
        let encoded = fs::read_to_string(&secret_path).with_context(|| {
            format!(
                "Failed to read OTP secret file {}",
                secret_path.display().to_string()
            )
        })?;
        let decrypted = store
            .decrypt(encoded.trim())
            .context("Failed to decrypt OTP secret file")?;
        Ok(Self::with_secret(config, decode_base32_secret(&decrypted)?))
    }

    fn with_secret(config: &OtpConfig, secret: Vec<u8>) -> Self {
        Self {
            config: config.clone(),
            secret,
            cached_codes: Mutex::new(HashMap::new()),
            consumed_codes: Mutex::new(HashMap::new()),
            single_use_failures: Mutex::new(SingleUseFailures::default()),
        }
    }

    pub fn from_config(
        config: &OtpConfig,
        zeroclaw_dir: &Path,
        store: &SecretStore,
    ) -> Result<(Self, Option<String>)> {
        let secret_path = secret_file_path(zeroclaw_dir);
        let (secret, generated) = if secret_path.exists() {
            let encoded = fs::read_to_string(&secret_path).with_context(|| {
                format!(
                    "Failed to read OTP secret file {}",
                    secret_path.display().to_string()
                )
            })?;
            let decrypted = store
                .decrypt(encoded.trim())
                .context("Failed to decrypt OTP secret file")?;
            (decode_base32_secret(&decrypted)?, false)
        } else {
            let raw: [u8; 20] = rand::random();
            let encoded_secret = encode_base32_secret(&raw);
            let encrypted = store
                .encrypt(&encoded_secret)
                .context("Failed to encrypt OTP secret")?;
            write_secret_file(&secret_path, &encrypted)?;
            (raw.to_vec(), true)
        };

        let validator = Self::with_secret(config, secret);
        let uri = if generated {
            Some(validator.otpauth_uri())
        } else {
            None
        };
        Ok((validator, uri))
    }

    pub fn validate(&self, code: &str) -> Result<bool> {
        self.validate_at(code, unix_timestamp_now())
    }

    fn validate_at(&self, code: &str, now_secs: u64) -> Result<bool> {
        let normalized = code.trim();
        if normalized.len() != OTP_DIGITS as usize
            || !normalized.chars().all(|ch| ch.is_ascii_digit())
        {
            return Ok(false);
        }

        {
            let mut cache = self.cached_codes.lock();
            cache.retain(|_, expiry| *expiry >= now_secs);
            if cache
                .get(normalized)
                .is_some_and(|expiry| *expiry >= now_secs)
            {
                return Ok(true);
            }
        }

        let step = self.config.token_ttl_secs.max(1);
        let counter = now_secs / step;
        let counters = [
            counter.saturating_sub(1),
            counter,
            counter.saturating_add(1),
        ];

        let is_valid = counters
            .iter()
            .map(|c| compute_totp_code(&self.secret, *c))
            .any(|candidate| candidate == normalized);

        if is_valid {
            let mut cache = self.cached_codes.lock();
            cache.insert(
                normalized.to_string(),
                now_secs.saturating_add(self.config.cache_valid_secs),
            );
        }

        Ok(is_valid)
    }

    /// Verify a code for an action that must not be replayable (emergency-stop
    /// resume). A code that validated once is consumed and refused again; the
    /// recently-validated cache used by [`Self::validate`] is neither read nor
    /// written. After `challenge_max_attempts` consecutive failures the
    /// verifier refuses every code for [`SINGLE_USE_LOCKOUT_SECS`].
    pub fn validate_single_use(&self, code: &str) -> Result<bool> {
        self.validate_single_use_at(code, unix_timestamp_now())
    }

    fn validate_single_use_at(&self, code: &str, now_secs: u64) -> Result<bool> {
        {
            let failures = self.single_use_failures.lock();
            if failures.locked_until > now_secs {
                anyhow::bail!(
                    "Too many failed OTP attempts; try again in {} seconds",
                    failures.locked_until - now_secs
                );
            }
        }

        let normalized = code.trim();
        let step = self.config.token_ttl_secs.max(1);
        let counter = now_secs / step;
        let well_formed = normalized.len() == OTP_DIGITS as usize
            && normalized.chars().all(|ch| ch.is_ascii_digit());
        let matches = well_formed
            && [
                counter.saturating_sub(1),
                counter,
                counter.saturating_add(1),
            ]
            .iter()
            .any(|c| compute_totp_code(&self.secret, *c) == normalized);

        let mut consumed = self.consumed_codes.lock();
        consumed.retain(|_, expiry| *expiry >= now_secs);
        let accepted = matches && !consumed.contains_key(normalized);
        if accepted {
            consumed.insert(
                normalized.to_string(),
                now_secs.saturating_add(step.saturating_mul(3)),
            );
        }
        drop(consumed);

        let mut failures = self.single_use_failures.lock();
        if accepted {
            *failures = SingleUseFailures::default();
        } else {
            failures.count = failures.count.saturating_add(1);
            if failures.count >= self.config.challenge_max_attempts.max(1) {
                failures.count = 0;
                failures.locked_until = now_secs.saturating_add(SINGLE_USE_LOCKOUT_SECS);
            }
        }
        Ok(accepted)
    }

    pub fn otpauth_uri(&self) -> String {
        let secret = encode_base32_secret(&self.secret);
        let account = "zeroclaw";
        format!(
            "otpauth://totp/{issuer}:{account}?secret={secret}&issuer={issuer}&period={period}",
            issuer = OTP_ISSUER,
            period = self.config.token_ttl_secs.max(1)
        )
    }

    /// The code an authenticator would show at `timestamp`. For tests that
    /// need a valid code for a secret that was enrolled by a real process.
    #[cfg(any(test, feature = "test-util"))]
    pub fn code_for_timestamp(&self, timestamp: u64) -> String {
        let counter = timestamp / self.config.token_ttl_secs.max(1);
        compute_totp_code(&self.secret, counter)
    }
}

pub fn secret_file_path(zeroclaw_dir: &Path) -> PathBuf {
    zeroclaw_dir.join(OTP_SECRET_FILE)
}

fn write_secret_file(path: &Path, value: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create directory {}",
                parent.display().to_string()
            )
        })?;
    }

    let temp_path = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    fs::write(&temp_path, value).with_context(|| {
        format!(
            "Failed to write temporary OTP secret {}",
            temp_path.display()
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o600));
    }

    fs::rename(&temp_path, path).with_context(|| {
        format!(
            "Failed to atomically replace OTP secret file {}",
            path.display()
        )
    })?;
    Ok(())
}

fn unix_timestamp_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn compute_totp_code(secret: &[u8], counter: u64) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret);
    let counter_bytes = counter.to_be_bytes();
    let digest = hmac::sign(&key, &counter_bytes);
    let hash = digest.as_ref();

    let offset = (hash[19] & 0x0f) as usize;
    let binary = ((u32::from(hash[offset]) & 0x7f) << 24)
        | (u32::from(hash[offset + 1]) << 16)
        | (u32::from(hash[offset + 2]) << 8)
        | u32::from(hash[offset + 3]);

    let code = binary % 10_u32.pow(OTP_DIGITS);
    format!("{code:0>6}")
}

fn encode_base32_secret(input: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    if input.is_empty() {
        return String::new();
    }

    let mut result = String::new();
    let mut buffer = 0u16;
    let mut bits_left = 0u8;

    for byte in input {
        buffer = (buffer << 8) | u16::from(*byte);
        bits_left += 8;

        while bits_left >= 5 {
            let index = ((buffer >> (bits_left - 5)) & 0x1f) as usize;
            result.push(ALPHABET[index] as char);
            bits_left -= 5;
        }
    }

    if bits_left > 0 {
        let index = ((buffer << (5 - bits_left)) & 0x1f) as usize;
        result.push(ALPHABET[index] as char);
    }

    result
}

fn decode_base32_secret(raw: &str) -> Result<Vec<u8>> {
    fn decode_char(ch: char) -> Option<u8> {
        match ch {
            'A'..='Z' => Some((ch as u8) - b'A'),
            '2'..='7' => Some((ch as u8) - b'2' + 26),
            _ => None,
        }
    }

    let mut cleaned = raw
        .chars()
        .filter(|ch| !matches!(ch, ' ' | '\t' | '\n' | '\r' | '-'))
        .collect::<String>()
        .to_ascii_uppercase();
    while cleaned.ends_with('=') {
        cleaned.pop();
    }
    if cleaned.is_empty() {
        anyhow::bail!("OTP secret is empty");
    }

    let mut output = Vec::new();
    let mut buffer = 0u32;
    let mut bits_left = 0u8;

    for ch in cleaned.chars() {
        let value = decode_char(ch)
            .with_context(|| format!("OTP secret contains invalid base32 character '{ch}'"))?;
        buffer = (buffer << 5) | u32::from(value);
        bits_left += 5;

        if bits_left >= 8 {
            let byte = ((buffer >> (bits_left - 8)) & 0xff) as u8;
            output.push(byte);
            bits_left -= 8;
        }
    }

    if output.is_empty() {
        anyhow::bail!("OTP secret did not decode to any bytes");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_config() -> OtpConfig {
        OtpConfig {
            enabled: true,
            token_ttl_secs: 30,
            cache_valid_secs: 120,
            ..OtpConfig::default()
        }
    }

    #[test]
    fn valid_totp_code_is_accepted() {
        let dir = tempdir().unwrap();
        let store = SecretStore::new(dir.path(), true);
        let (validator, _) = OtpValidator::from_config(&test_config(), dir.path(), &store).unwrap();

        let now = 1_700_000_000u64;
        let code = validator.code_for_timestamp(now);
        assert!(validator.validate_at(&code, now).unwrap());
    }

    #[test]
    fn expired_totp_code_is_rejected() {
        let dir = tempdir().unwrap();
        let store = SecretStore::new(dir.path(), true);
        let (validator, _) = OtpValidator::from_config(&test_config(), dir.path(), &store).unwrap();

        let stale = 1_700_000_000u64;
        let now = stale + 300;
        let code = validator.code_for_timestamp(stale);
        assert!(!validator.validate_at(&code, now).unwrap());
    }

    #[test]
    fn wrong_totp_code_is_rejected() {
        let dir = tempdir().unwrap();
        let store = SecretStore::new(dir.path(), true);
        let (validator, _) = OtpValidator::from_config(&test_config(), dir.path(), &store).unwrap();
        assert!(!validator.validate_at("123456", 1_700_000_000).unwrap());
    }

    #[test]
    fn secret_is_generated_and_reused() {
        let dir = tempdir().unwrap();
        let store = SecretStore::new(dir.path(), true);

        let (first, first_uri) =
            OtpValidator::from_config(&test_config(), dir.path(), &store).unwrap();
        assert!(first_uri.is_some());

        let secret_path = secret_file_path(dir.path());
        let stored = fs::read_to_string(&secret_path).unwrap();
        assert!(SecretStore::is_encrypted(stored.trim()));

        let (second, second_uri) =
            OtpValidator::from_config(&test_config(), dir.path(), &store).unwrap();
        assert!(second_uri.is_none());

        let ts = 1_700_000_000u64;
        assert_eq!(first.code_for_timestamp(ts), second.code_for_timestamp(ts));
    }

    fn single_use_validator(dir: &Path) -> OtpValidator {
        let store = SecretStore::new(dir, true);
        let config = OtpConfig {
            challenge_max_attempts: 3,
            ..test_config()
        };
        OtpValidator::from_config(&config, dir, &store).unwrap().0
    }

    /// A well-formed code that no accepted step (`counter - 1 ..= counter + 1`)
    /// produces at `now`.
    fn wrong_code(validator: &OtpValidator, now: u64) -> String {
        let accepted = [
            validator.code_for_timestamp(now.saturating_sub(30)),
            validator.code_for_timestamp(now),
            validator.code_for_timestamp(now + 30),
        ];
        (0..)
            .map(|n| format!("{n:06}"))
            .find(|candidate| !accepted.contains(candidate))
            .unwrap()
    }

    #[test]
    fn single_use_code_is_consumed_by_its_first_acceptance() {
        let dir = tempdir().unwrap();
        let validator = single_use_validator(dir.path());
        let now = 1_700_000_000u64;
        let code = validator.code_for_timestamp(now);

        assert!(validator.validate_single_use_at(&code, now).unwrap());
        assert!(
            !validator.validate_single_use_at(&code, now).unwrap(),
            "the same code must not validate twice"
        );
        assert!(
            !validator.validate_single_use_at(&code, now + 30).unwrap(),
            "nor in the next step, where it is still inside the accepted window"
        );
    }

    #[test]
    fn single_use_verification_locks_after_the_configured_failures() {
        let dir = tempdir().unwrap();
        let validator = single_use_validator(dir.path());
        let now = 1_700_000_000u64;
        let wrong = wrong_code(&validator, now);

        // Three consecutive failures (challenge_max_attempts = 3) lock it.
        for _ in 0..3 {
            assert!(!validator.validate_single_use_at(&wrong, now).unwrap());
        }
        // While locked even a valid code is refused, with an error.
        let valid = validator.code_for_timestamp(now + 1);
        let error = validator
            .validate_single_use_at(&valid, now + 1)
            .expect_err("a locked verifier must refuse every code");
        assert!(error.to_string().contains("Too many failed OTP attempts"));
        let error = validator
            .validate_single_use_at(&valid, now + SINGLE_USE_LOCKOUT_SECS - 1)
            .expect_err("still locked one second before the lockout ends");
        assert!(error.to_string().contains("Too many failed OTP attempts"));

        // The lockout ends after SINGLE_USE_LOCKOUT_SECS.
        let later = now + SINGLE_USE_LOCKOUT_SECS;
        let code = validator.code_for_timestamp(later);
        assert!(validator.validate_single_use_at(&code, later).unwrap());
    }

    #[test]
    fn a_successful_single_use_code_resets_the_failure_count() {
        let dir = tempdir().unwrap();
        let validator = single_use_validator(dir.path());
        let now = 1_700_000_000u64;
        let wrong = wrong_code(&validator, now);

        for _ in 0..2 {
            assert!(!validator.validate_single_use_at(&wrong, now).unwrap());
        }
        let code = validator.code_for_timestamp(now);
        assert!(validator.validate_single_use_at(&code, now).unwrap());
        for _ in 0..2 {
            assert!(!validator.validate_single_use_at(&wrong, now).unwrap());
        }
        // Two failures after the reset: not locked, so a fresh valid code works.
        let later = now + 90;
        let fresh = validator.code_for_timestamp(later);
        assert!(validator.validate_single_use_at(&fresh, later).unwrap());
    }

    #[test]
    fn from_existing_never_creates_a_secret() {
        let dir = tempdir().unwrap();
        let store = SecretStore::new(dir.path(), true);

        let error = OtpValidator::from_existing(&test_config(), dir.path(), &store)
            .expect_err("no secret is enrolled yet");
        assert!(error.to_string().contains("No OTP secret is enrolled"));
        assert!(
            !secret_file_path(dir.path()).exists(),
            "from_existing must not mint a secret"
        );

        let (enrolled, _) = OtpValidator::from_config(&test_config(), dir.path(), &store).unwrap();
        let existing = OtpValidator::from_existing(&test_config(), dir.path(), &store).unwrap();
        let ts = 1_700_000_000u64;
        assert_eq!(
            enrolled.code_for_timestamp(ts),
            existing.code_for_timestamp(ts)
        );
    }
}
