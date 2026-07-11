use std::collections::{HashMap, HashSet};
use std::fmt;

use sha2::{Digest, Sha256};

use worker::{Env, Url};

use crate::error::{ApiError, ApiResult};

const DEFAULT_ELEVENLABS_BASE_URL: &str = "https://api.elevenlabs.io/v1";
const DEFAULT_OPENAI_STT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_MAX_AUDIO_BYTES: usize = 4 * 1024 * 1024;
const HARD_MAX_AUDIO_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_MAX_HERMES_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const HARD_MAX_HERMES_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MIN_CONVERSATION_IDLE_SECONDS: u64 = 60;
const MAX_CONVERSATION_IDLE_SECONDS: u64 = 365 * 24 * 60 * 60;
const DEFAULT_CONVERSATION_IDLE_SECONDS: u64 = 15 * 60;
const MIN_DEVICE_TOKEN_BYTES: usize = 32;
const MAX_DEVICE_TOKEN_BYTES: usize = 512;
const DEFAULT_REALTIME_MAX_AUDIO_SECONDS: u32 = 30;
const HARD_REALTIME_MAX_AUDIO_SECONDS: u32 = 30;
const DEFAULT_REALTIME_MAX_OUTPUT_SECONDS: u32 = 30;
const HARD_REALTIME_MAX_OUTPUT_SECONDS: u32 = 30;
const DEFAULT_MAX_CONNECTION_AUDIO_SECONDS: u32 = 15 * 60;
const HARD_MAX_CONNECTION_AUDIO_SECONDS: u32 = 24 * 60 * 60;
const DEFAULT_MAX_TURNS_PER_CONNECTION: u32 = 256;
const HARD_MAX_TURNS_PER_CONNECTION: u32 = 10_000;
const DEFAULT_MAX_MESSAGES_PER_CONNECTION: u32 = 16_384;
const HARD_MAX_MESSAGES_PER_CONNECTION: u32 = 1_000_000;
pub const DEFAULT_VOICE_INSTRUCTIONS: &str = "Give a concise, natural spoken answer suitable for text-to-speech. Do not use Markdown, code blocks, tables, or raw URLs unless the user explicitly requests them. Put natural sentence punctuation early enough that speech can begin before the whole answer is complete.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SttProvider {
    ElevenLabs,
    OpenAiCompatible,
}

impl SttProvider {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ElevenLabs => "elevenlabs",
            Self::OpenAiCompatible => "openai-compatible",
        }
    }
}

#[derive(Clone)]
enum DeviceAuth {
    Shared(String),
    PerDevice(HashMap<String, String>),
}

#[derive(Clone)]
pub struct Config {
    device_auth: DeviceAuth,
    hermes_session_keys: HashMap<String, String>,
    pub max_audio_bytes: usize,
    pub max_audio_seconds: u32,
    pub realtime_max_audio_seconds: u32,
    pub realtime_max_output_seconds: u32,
    pub max_connection_audio_seconds: u32,
    pub max_turns_per_connection: u32,
    pub max_messages_per_connection: u32,
    pub max_hermes_response_bytes: usize,
    pub stt_provider: SttProvider,
    pub stt_model: String,
    pub realtime_stt_model: String,
    pub stt_language_code: Option<String>,
    pub stt_base_url: String,
    pub stt_api_key: String,
    pub elevenlabs_base_url: String,
    pub elevenlabs_api_key: String,
    pub elevenlabs_voice_id: String,
    pub tts_model: String,
    pub elevenlabs_enable_logging: bool,
    pub hermes_base_url: String,
    pub hermes_api_key: String,
    pub hermes_model: String,
    pub hermes_profile_id: String,
    pub hermes_binding_revision: String,
    pub hermes_voice_instructions: String,
    pub conversation_idle_seconds: Option<u64>,
    pub cf_access_client_id: Option<String>,
    pub cf_access_client_secret: Option<String>,
    pub diagnostic_v1_enabled: bool,
}

impl fmt::Debug for Config {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Config")
            .field("device_auth", &"<redacted>")
            .field("hermes_session_keys", &"<redacted>")
            .field("max_audio_bytes", &self.max_audio_bytes)
            .field("max_audio_seconds", &self.max_audio_seconds)
            .field(
                "realtime_max_audio_seconds",
                &self.realtime_max_audio_seconds,
            )
            .field(
                "realtime_max_output_seconds",
                &self.realtime_max_output_seconds,
            )
            .field(
                "max_connection_audio_seconds",
                &self.max_connection_audio_seconds,
            )
            .field("max_turns_per_connection", &self.max_turns_per_connection)
            .field(
                "max_messages_per_connection",
                &self.max_messages_per_connection,
            )
            .field("stt_provider", &self.stt_provider)
            .field("stt_model", &self.stt_model)
            .field("realtime_stt_model", &self.realtime_stt_model)
            .field("stt_language_code", &self.stt_language_code)
            .field("stt_base_url", &"<redacted-origin>")
            .field("stt_api_key", &"<redacted>")
            .field("elevenlabs_base_url", &"<redacted-origin>")
            .field("elevenlabs_api_key", &"<redacted>")
            .field("elevenlabs_voice_id", &self.elevenlabs_voice_id)
            .field("tts_model", &self.tts_model)
            .field("elevenlabs_enable_logging", &self.elevenlabs_enable_logging)
            .field("hermes_base_url", &"<redacted-origin>")
            .field("hermes_api_key", &"<redacted>")
            .field("hermes_model", &self.hermes_model)
            .field("hermes_profile_id", &self.hermes_profile_id)
            .field("hermes_binding_revision", &self.hermes_binding_revision)
            .field("hermes_voice_instructions", &"<redacted>")
            .field("conversation_idle_seconds", &self.conversation_idle_seconds)
            .field("cf_access_client_id", &"<redacted>")
            .field("cf_access_client_secret", &"<redacted>")
            .field("diagnostic_v1_enabled", &self.diagnostic_v1_enabled)
            .finish()
    }
}

impl Config {
    pub fn from_env(env: &Env) -> ApiResult<Self> {
        let device_auth = load_device_auth(env)?;
        let hermes_session_keys = load_hermes_session_keys(env)?;
        let allow_implicit_hermes_context =
            parse_bool(optional_var(env, "ALLOW_IMPLICIT_HERMES_CONTEXT"), false)?;
        if !allow_implicit_hermes_context
            && !device_auth.has_explicit_context_for_every_device(&hermes_session_keys)
        {
            return Err(ApiError::configuration());
        }
        let stt_provider = match optional_var(env, "STT_PROVIDER")
            .unwrap_or_else(|| "elevenlabs".to_string())
            .to_ascii_lowercase()
            .as_str()
        {
            "elevenlabs" => SttProvider::ElevenLabs,
            "openai-compatible" | "openai" => SttProvider::OpenAiCompatible,
            _ => return Err(ApiError::configuration()),
        };

        let elevenlabs_api_key = required_secret(env, "ELEVENLABS_API_KEY")?;
        let elevenlabs_base_url = normalize_base_url(
            &optional_var(env, "ELEVENLABS_BASE_URL")
                .unwrap_or_else(|| DEFAULT_ELEVENLABS_BASE_URL.to_string()),
        )?;

        let stt_model_override = optional_var(env, "STT_MODEL");
        let (stt_model, stt_base_url, stt_api_key) = match stt_provider {
            SttProvider::ElevenLabs => (
                stt_model_override.unwrap_or_else(|| "scribe_v2".to_string()),
                elevenlabs_base_url.clone(),
                elevenlabs_api_key.clone(),
            ),
            SttProvider::OpenAiCompatible => (
                stt_model_override.unwrap_or_else(|| "whisper-1".to_string()),
                normalize_base_url(
                    &optional_var(env, "STT_BASE_URL")
                        .unwrap_or_else(|| DEFAULT_OPENAI_STT_BASE_URL.to_string()),
                )?,
                required_secret(env, "STT_API_KEY")?,
            ),
        };

        validate_identifier(&stt_model, 160)?;
        let realtime_stt_model = optional_var(env, "REALTIME_STT_MODEL")
            .unwrap_or_else(|| "scribe_v2_realtime".to_string());
        validate_identifier(&realtime_stt_model, 160)?;

        let stt_language_code = optional_var(env, "STT_LANGUAGE_CODE");
        if let Some(language_code) = stt_language_code.as_deref() {
            if language_code.len() > 16
                || language_code.is_empty()
                || !language_code
                    .bytes()
                    .all(|byte| byte.is_ascii_alphabetic() || byte == b'-')
            {
                return Err(ApiError::configuration());
            }
        }

        let max_audio_bytes = parse_bounded_usize(
            optional_var(env, "MAX_AUDIO_BYTES"),
            DEFAULT_MAX_AUDIO_BYTES,
            44,
            HARD_MAX_AUDIO_BYTES,
        )?;
        let max_audio_seconds =
            parse_bounded_u32(optional_var(env, "MAX_AUDIO_SECONDS"), 120, 1, 600)?;
        // Scribe realtime manual sessions auto-commit at roughly 36 seconds,
        // while the Voice PE's 1 MiB capture ring holds about 32.7 seconds.
        // Until committed-segment accumulation is implemented, keep one turn
        // strictly below both boundaries.
        let realtime_max_audio_seconds = parse_bounded_u32(
            optional_var(env, "REALTIME_MAX_AUDIO_SECONDS"),
            DEFAULT_REALTIME_MAX_AUDIO_SECONDS,
            1,
            HARD_REALTIME_MAX_AUDIO_SECONDS,
        )?;
        // Realtime replies share the same hard 30-second room-audio safety
        // contract as microphone capture. A provider must not be able to turn
        // a short Hermes answer into an arbitrarily long playback stream.
        let realtime_max_output_seconds = parse_bounded_u32(
            optional_var(env, "REALTIME_MAX_OUTPUT_SECONDS"),
            DEFAULT_REALTIME_MAX_OUTPUT_SECONDS,
            1,
            HARD_REALTIME_MAX_OUTPUT_SECONDS,
        )?;
        let max_connection_audio_seconds = parse_bounded_u32(
            optional_var(env, "MAX_CONNECTION_AUDIO_SECONDS"),
            DEFAULT_MAX_CONNECTION_AUDIO_SECONDS,
            realtime_max_audio_seconds,
            HARD_MAX_CONNECTION_AUDIO_SECONDS,
        )?;
        let max_turns_per_connection = parse_bounded_u32(
            optional_var(env, "MAX_TURNS_PER_CONNECTION"),
            DEFAULT_MAX_TURNS_PER_CONNECTION,
            1,
            HARD_MAX_TURNS_PER_CONNECTION,
        )?;
        let max_messages_per_connection = parse_bounded_u32(
            optional_var(env, "MAX_MESSAGES_PER_CONNECTION"),
            DEFAULT_MAX_MESSAGES_PER_CONNECTION,
            128,
            HARD_MAX_MESSAGES_PER_CONNECTION,
        )?;
        let max_hermes_response_bytes = parse_bounded_usize(
            optional_var(env, "MAX_HERMES_RESPONSE_BYTES"),
            DEFAULT_MAX_HERMES_RESPONSE_BYTES,
            64 * 1024,
            HARD_MAX_HERMES_RESPONSE_BYTES,
        )?;

        let elevenlabs_voice_id = required_var(env, "ELEVENLABS_VOICE_ID")?;
        validate_identifier(&elevenlabs_voice_id, 128)?;
        let tts_model =
            optional_var(env, "TTS_MODEL").unwrap_or_else(|| "eleven_flash_v2_5".to_string());
        validate_identifier(&tts_model, 160)?;
        let elevenlabs_enable_logging =
            parse_bool(optional_var(env, "ELEVENLABS_ENABLE_LOGGING"), true)?;

        let hermes_base_url = normalize_base_url(&required_var(env, "HERMES_BASE_URL")?)?;
        let hermes_api_key = required_secret(env, "HERMES_API_KEY")?;
        let hermes_model =
            optional_var(env, "HERMES_MODEL").unwrap_or_else(|| "hermes-agent".to_string());
        validate_identifier(&hermes_model, 160)?;
        let hermes_profile_id =
            optional_var(env, "HERMES_PROFILE_ID").unwrap_or_else(|| "default".to_string());
        validate_identifier(&hermes_profile_id, 160)?;
        let hermes_binding_revision =
            optional_var(env, "HERMES_BINDING_REVISION").unwrap_or_else(|| "v1".to_string());
        validate_identifier(&hermes_binding_revision, 160)?;
        let hermes_voice_instructions = optional_secret(env, "HERMES_VOICE_INSTRUCTIONS")
            .or_else(|| optional_var(env, "HERMES_VOICE_INSTRUCTIONS"))
            .unwrap_or_else(|| DEFAULT_VOICE_INSTRUCTIONS.to_string());
        validate_instruction_text(&hermes_voice_instructions)?;
        let conversation_idle_seconds = match optional_var(env, "CONVERSATION_IDLE_SECONDS") {
            Some(value) => parse_conversation_idle_seconds(Some(value))?,
            None => Some(DEFAULT_CONVERSATION_IDLE_SECONDS),
        };
        if conversation_idle_seconds.is_none()
            && !parse_bool(optional_var(env, "ALLOW_UNBOUNDED_CONVERSATION"), false)?
        {
            return Err(ApiError::configuration());
        }

        // Cloudflare presents both plaintext string vars and encrypted secrets
        // as the same runtime string binding. The application can validate the
        // pair but cannot prove how it was deployed; CI/config review must keep
        // CF_ACCESS_CLIENT_SECRET out of wrangler.toml and use `wrangler secret`.
        let cf_access_client_id = optional_secret(env, "CF_ACCESS_CLIENT_ID");
        let cf_access_client_secret = optional_secret(env, "CF_ACCESS_CLIENT_SECRET");
        if cf_access_client_id.is_some() != cf_access_client_secret.is_some() {
            return Err(ApiError::configuration());
        }

        let diagnostic_v1_enabled = parse_bool(optional_var(env, "DIAGNOSTIC_V1_ENABLED"), false)?;

        Ok(Self {
            device_auth,
            hermes_session_keys,
            max_audio_bytes,
            max_audio_seconds,
            realtime_max_audio_seconds,
            realtime_max_output_seconds,
            max_connection_audio_seconds,
            max_turns_per_connection,
            max_messages_per_connection,
            max_hermes_response_bytes,
            stt_provider,
            stt_model,
            realtime_stt_model,
            stt_language_code,
            stt_base_url,
            stt_api_key,
            elevenlabs_base_url,
            elevenlabs_api_key,
            elevenlabs_voice_id,
            tts_model,
            elevenlabs_enable_logging,
            hermes_base_url,
            hermes_api_key,
            hermes_model,
            hermes_profile_id,
            hermes_binding_revision,
            hermes_voice_instructions,
            conversation_idle_seconds,
            cf_access_client_id,
            cf_access_client_secret,
            diagnostic_v1_enabled,
        })
    }

    pub fn authenticate(&self, device_id: &str, supplied_token: &str) -> bool {
        let expected = match &self.device_auth {
            DeviceAuth::Shared(token) => Some(token.as_str()),
            DeviceAuth::PerDevice(tokens) => tokens.get(device_id).map(String::as_str),
        };

        expected
            .map(|token| constant_time_token_eq(token, supplied_token))
            .unwrap_or(false)
    }

    /// Authenticate and return a non-secret credential epoch suitable for a
    /// hibernating WebSocket attachment. Recomputing this on each callback
    /// makes token rotation revoke already-established sockets.
    pub fn authenticate_fingerprint(
        &self,
        device_id: &str,
        supplied_token: &str,
    ) -> Option<String> {
        self.authenticate(device_id, supplied_token)
            .then(|| token_fingerprint(supplied_token))
    }

    pub fn expected_token_fingerprint(&self, device_id: &str) -> Option<String> {
        match &self.device_auth {
            DeviceAuth::Shared(token) => Some(token_fingerprint(token)),
            DeviceAuth::PerDevice(tokens) => {
                tokens.get(device_id).map(|token| token_fingerprint(token))
            }
        }
    }

    pub fn hermes_session_key(&self, device_id: &str) -> String {
        self.hermes_session_keys
            .get(device_id)
            .cloned()
            .unwrap_or_else(|| format!("voice:{device_id}"))
    }

    pub fn diagnostic_hermes_session_key(&self, device_id: &str) -> String {
        format!(
            "diagnostic:{}",
            token_fingerprint(&self.hermes_session_key(device_id))
        )
    }
}

impl DeviceAuth {
    fn has_explicit_context_for_every_device(
        &self,
        session_keys: &HashMap<String, String>,
    ) -> bool {
        match self {
            Self::PerDevice(tokens) => tokens
                .keys()
                .all(|device_id| session_keys.contains_key(device_id)),
            // A shared credential accepts arbitrary device IDs, so no finite
            // map can prove complete coverage. Its implicit memory scope must
            // be separately acknowledged by the operator.
            Self::Shared(_) => false,
        }
    }
}
pub fn valid_device_id(device_id: &str) -> bool {
    let bytes = device_id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn load_device_auth(env: &Env) -> ApiResult<DeviceAuth> {
    if let Some(json) = optional_secret(env, "DEVICE_TOKENS_JSON") {
        let tokens: HashMap<String, String> =
            serde_json::from_str(&json).map_err(|_| ApiError::configuration())?;
        if tokens.is_empty()
            || tokens
                .iter()
                .any(|(device_id, token)| !valid_device_id(device_id) || !valid_device_token(token))
            || tokens.values().collect::<HashSet<_>>().len() != tokens.len()
        {
            return Err(ApiError::configuration());
        }
        return Ok(DeviceAuth::PerDevice(tokens));
    }

    if !parse_bool(optional_var(env, "ALLOW_SHARED_DEVICE_TOKEN"), false)? {
        return Err(ApiError::configuration());
    }
    let token = required_secret(env, "DEVICE_AUTH_TOKEN")?;
    if !valid_device_token(&token) {
        return Err(ApiError::configuration());
    }
    Ok(DeviceAuth::Shared(token))
}

fn load_hermes_session_keys(env: &Env) -> ApiResult<HashMap<String, String>> {
    let Some(json) = optional_secret(env, "HERMES_SESSION_KEYS_JSON") else {
        return Ok(HashMap::new());
    };
    let keys: HashMap<String, String> =
        serde_json::from_str(&json).map_err(|_| ApiError::configuration())?;
    if keys.iter().any(|(device_id, session_key)| {
        !valid_device_id(device_id) || !valid_hermes_session_key(session_key)
    }) {
        return Err(ApiError::configuration());
    }
    Ok(keys)
}

fn required_secret(env: &Env, name: &str) -> ApiResult<String> {
    optional_secret(env, name).ok_or_else(ApiError::configuration)
}

fn optional_secret(env: &Env, name: &str) -> Option<String> {
    env.secret(name)
        .ok()
        .map(|secret| secret.to_string())
        .filter(|value| !value.is_empty())
}

fn required_var(env: &Env, name: &str) -> ApiResult<String> {
    optional_var(env, name).ok_or_else(ApiError::configuration)
}

fn optional_var(env: &Env, name: &str) -> Option<String> {
    env.var(name)
        .ok()
        .map(|value| value.to_string())
        .filter(|value| !value.trim().is_empty())
}

fn normalize_base_url(value: &str) -> ApiResult<String> {
    let trimmed = value.trim().trim_end_matches('/');
    let url = Url::parse(trimmed).map_err(|_| ApiError::configuration())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ApiError::configuration());
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

fn validate_identifier(value: &str, max_len: usize) -> ApiResult<()> {
    if value.is_empty()
        || value.len() > max_len
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/' | b':' | b'@')
        })
    {
        return Err(ApiError::configuration());
    }
    Ok(())
}

fn parse_bounded_usize(
    value: Option<String>,
    default: usize,
    minimum: usize,
    maximum: usize,
) -> ApiResult<usize> {
    let parsed = value
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| ApiError::configuration())?
        .unwrap_or(default);
    if !(minimum..=maximum).contains(&parsed) {
        return Err(ApiError::configuration());
    }
    Ok(parsed)
}

fn parse_bounded_u32(
    value: Option<String>,
    default: u32,
    minimum: u32,
    maximum: u32,
) -> ApiResult<u32> {
    let parsed = value
        .map(|value| value.parse::<u32>())
        .transpose()
        .map_err(|_| ApiError::configuration())?
        .unwrap_or(default);
    if !(minimum..=maximum).contains(&parsed) {
        return Err(ApiError::configuration());
    }
    Ok(parsed)
}

fn parse_bool(value: Option<String>, default: bool) -> ApiResult<bool> {
    match value.as_deref().map(str::trim) {
        None => Ok(default),
        Some("1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON") => Ok(true),
        Some("0" | "false" | "FALSE" | "no" | "NO" | "off" | "OFF") => Ok(false),
        Some(_) => Err(ApiError::configuration()),
    }
}

fn validate_instruction_text(value: &str) -> ApiResult<()> {
    if value.trim().is_empty()
        || value.len() > 2_048
        || value.chars().any(|character| {
            character == '\0' || (character.is_control() && !character.is_whitespace())
        })
    {
        return Err(ApiError::configuration());
    }
    Ok(())
}

fn parse_conversation_idle_seconds(value: Option<String>) -> ApiResult<Option<u64>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value == "0" || value.eq_ignore_ascii_case("off") || value.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let seconds = value
        .parse::<u64>()
        .map_err(|_| ApiError::configuration())?;
    if !(MIN_CONVERSATION_IDLE_SECONDS..=MAX_CONVERSATION_IDLE_SECONDS).contains(&seconds) {
        return Err(ApiError::configuration());
    }
    Ok(Some(seconds))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let compared_len = left.len().max(right.len());
    for index in 0..compared_len {
        let left_byte = left.get(index).copied().unwrap_or(0);
        let right_byte = right.get(index).copied().unwrap_or(0);
        difference |= usize::from(left_byte ^ right_byte);
    }
    difference == 0
}

fn constant_time_token_eq(expected: &str, supplied: &str) -> bool {
    let expected = Sha256::digest(expected.as_bytes());
    let supplied = Sha256::digest(supplied.as_bytes());
    constant_time_eq(expected.as_slice(), supplied.as_slice())
}

fn token_fingerprint(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn valid_device_token(token: &str) -> bool {
    (MIN_DEVICE_TOKEN_BYTES..=MAX_DEVICE_TOKEN_BYTES).contains(&token.len())
        && token.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

fn valid_hermes_session_key(value: &str) -> bool {
    !value.is_empty()
        && value == value.trim()
        && value.len() <= 256
        && !value.bytes().any(|byte| byte.is_ascii_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_device_ids() {
        assert!(valid_device_id("voice-pe.kitchen_1"));
        assert!(!valid_device_id(""));
        assert!(!valid_device_id("-starts-with-dash"));
        assert!(!valid_device_id("contains/slash"));
        assert!(!valid_device_id(&"a".repeat(65)));
    }

    #[test]
    fn base_urls_require_https_and_a_host() {
        assert!(normalize_base_url("https://example.com/v1").is_ok());
        assert_eq!(
            normalize_base_url("HTTPS://EXAMPLE.COM/").unwrap(),
            "https://example.com"
        );
        assert!(normalize_base_url("http://127.0.0.1:8642").is_err());
        assert!(normalize_base_url("file:///tmp/hermes").is_err());
        assert!(normalize_base_url("https://").is_err());
        assert!(normalize_base_url("https://user@example.com").is_err());
    }

    #[test]
    fn compares_tokens() {
        assert!(constant_time_eq(b"same-token", b"same-token"));
        assert!(!constant_time_eq(b"same-token", b"other-token"));
        assert!(!constant_time_eq(b"short", b"much-longer"));
        assert!(constant_time_token_eq("same-token", "same-token"));
        assert!(!constant_time_token_eq("same-token", "same-tokeN"));
        assert!(!constant_time_token_eq("short", "much-longer"));
    }

    #[test]
    fn credential_fingerprints_are_fixed_width_and_non_reversible_bearers() {
        let fingerprint = token_fingerprint("0123456789abcdef");
        assert_eq!(fingerprint.len(), 64);
        assert!(fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(fingerprint, "0123456789abcdef");
        assert_ne!(
            fingerprint,
            token_fingerprint("0123456789abcdeg"),
            "credential rotation must produce a new socket epoch"
        );
    }

    #[test]
    fn implicit_memory_context_is_not_accepted_by_default_policy() {
        let tokens = DeviceAuth::PerDevice(HashMap::from([
            ("kitchen".into(), "0123456789abcdef".into()),
            ("office".into(), "fedcba9876543210".into()),
        ]));
        let complete = HashMap::from([
            ("kitchen".into(), "memory:kitchen".into()),
            ("office".into(), "memory:office".into()),
        ]);
        assert!(tokens.has_explicit_context_for_every_device(&complete));

        let missing = HashMap::from([("kitchen".into(), "memory:kitchen".into())]);
        assert!(!tokens.has_explicit_context_for_every_device(&missing));
        assert!(!DeviceAuth::Shared("0123456789abcdef".into())
            .has_explicit_context_for_every_device(&complete));
    }

    #[test]
    fn parses_only_explicit_boolean_spellings() {
        for value in ["1", "true", "yes", "on"] {
            assert!(parse_bool(Some(value.into()), false).unwrap());
        }
        for value in ["0", "false", "no", "off"] {
            assert!(!parse_bool(Some(value.into()), true).unwrap());
        }
        assert!(parse_bool(Some("perhaps".into()), false).is_err());
        assert!(parse_bool(None, true).unwrap());
    }

    #[test]
    fn enforces_device_token_length_and_printable_content() {
        assert!(valid_device_token("0123456789abcdef0123456789abcdef"));
        assert!(!valid_device_token("0123456789abcdef"));
        assert!(!valid_device_token("too-short"));
        assert!(!valid_device_token("0123456789abcde\n"));
        assert!(!valid_device_token("0123456789abcdef 123456789abcdef"));
        assert!(!valid_device_token("0123456789abcdef0123456789abcdeé"));
        assert!(!valid_device_token(&"x".repeat(513)));
    }

    #[test]
    fn conversation_idle_parser_supports_bounded_and_explicit_off_values() {
        assert_eq!(parse_conversation_idle_seconds(None).unwrap(), None);
        assert_eq!(
            parse_conversation_idle_seconds(Some("0".into())).unwrap(),
            None
        );
        assert_eq!(
            parse_conversation_idle_seconds(Some("off".into())).unwrap(),
            None
        );
        assert_eq!(
            parse_conversation_idle_seconds(Some("1800".into())).unwrap(),
            Some(1800)
        );
        assert!(parse_conversation_idle_seconds(Some("59".into())).is_err());
        assert!(parse_conversation_idle_seconds(Some("31536001".into())).is_err());
        assert!(parse_conversation_idle_seconds(Some("tomorrow".into())).is_err());
    }

    #[test]
    fn realtime_output_duration_has_a_hard_thirty_second_ceiling() {
        assert_eq!(
            parse_bounded_u32(
                Some("30".into()),
                DEFAULT_REALTIME_MAX_OUTPUT_SECONDS,
                1,
                HARD_REALTIME_MAX_OUTPUT_SECONDS,
            )
            .unwrap(),
            30
        );
        assert!(parse_bounded_u32(
            Some("31".into()),
            DEFAULT_REALTIME_MAX_OUTPUT_SECONDS,
            1,
            HARD_REALTIME_MAX_OUTPUT_SECONDS,
        )
        .is_err());
    }

    #[test]
    fn validates_worker_owned_hermes_memory_scopes() {
        assert!(valid_hermes_session_key("agent:main:voice:room:kitchen"));
        assert!(!valid_hermes_session_key(""));
        assert!(!valid_hermes_session_key("   "));
        assert!(!valid_hermes_session_key(" room:kitchen"));
        assert!(!valid_hermes_session_key("room:kitchen "));
        assert!(!valid_hermes_session_key("room\nother"));
        assert!(!valid_hermes_session_key(&"x".repeat(257)));
    }

    #[test]
    fn validates_operator_voice_instructions_without_control_bytes() {
        assert!(validate_instruction_text(DEFAULT_VOICE_INSTRUCTIONS).is_ok());
        assert!(validate_instruction_text(" ").is_err());
        assert!(validate_instruction_text("unsafe\0suffix").is_err());
        assert!(validate_instruction_text(&"x".repeat(2_049)).is_err());
    }
}
