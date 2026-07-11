use std::collections::HashMap;

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
const MIN_DEVICE_TOKEN_BYTES: usize = 16;
const MAX_DEVICE_TOKEN_BYTES: usize = 512;

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

#[derive(Clone, Debug)]
enum DeviceAuth {
    Shared(String),
    PerDevice(HashMap<String, String>),
}

#[derive(Clone, Debug)]
pub struct Config {
    device_auth: DeviceAuth,
    hermes_session_keys: HashMap<String, String>,
    pub max_audio_bytes: usize,
    pub max_audio_seconds: u32,
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
    pub hermes_base_url: String,
    pub hermes_api_key: String,
    pub hermes_model: String,
    pub conversation_idle_seconds: Option<u64>,
    pub cf_access_client_id: Option<String>,
    pub cf_access_client_secret: Option<String>,
}

impl Config {
    pub fn from_env(env: &Env) -> ApiResult<Self> {
        let device_auth = load_device_auth(env)?;
        let hermes_session_keys = load_hermes_session_keys(env)?;
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

        let hermes_base_url = normalize_base_url(&required_var(env, "HERMES_BASE_URL")?)?;
        let hermes_api_key = required_secret(env, "HERMES_API_KEY")?;
        let hermes_model =
            optional_var(env, "HERMES_MODEL").unwrap_or_else(|| "hermes-agent".to_string());
        validate_identifier(&hermes_model, 160)?;
        let conversation_idle_seconds =
            parse_conversation_idle_seconds(optional_var(env, "CONVERSATION_IDLE_SECONDS"))?;

        let cf_access_client_id = optional_binding(env, "CF_ACCESS_CLIENT_ID");
        let cf_access_client_secret = optional_binding(env, "CF_ACCESS_CLIENT_SECRET");
        if cf_access_client_id.is_some() != cf_access_client_secret.is_some() {
            return Err(ApiError::configuration());
        }

        Ok(Self {
            device_auth,
            hermes_session_keys,
            max_audio_bytes,
            max_audio_seconds,
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
            hermes_base_url,
            hermes_api_key,
            hermes_model,
            conversation_idle_seconds,
            cf_access_client_id,
            cf_access_client_secret,
        })
    }

    pub fn authenticate(&self, device_id: &str, supplied_token: &str) -> bool {
        let expected = match &self.device_auth {
            DeviceAuth::Shared(token) => Some(token.as_str()),
            DeviceAuth::PerDevice(tokens) => tokens.get(device_id).map(String::as_str),
        };

        expected
            .map(|token| constant_time_eq(token.as_bytes(), supplied_token.as_bytes()))
            .unwrap_or(false)
    }

    pub fn hermes_session_key(&self, device_id: &str) -> String {
        self.hermes_session_keys
            .get(device_id)
            .cloned()
            .unwrap_or_else(|| format!("voice:{device_id}"))
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
        {
            return Err(ApiError::configuration());
        }
        return Ok(DeviceAuth::PerDevice(tokens));
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

fn optional_binding(env: &Env, name: &str) -> Option<String> {
    optional_secret(env, name).or_else(|| optional_var(env, name))
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
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

fn valid_device_token(token: &str) -> bool {
    (MIN_DEVICE_TOKEN_BYTES..=MAX_DEVICE_TOKEN_BYTES).contains(&token.len())
        && !token.bytes().any(|byte| byte.is_ascii_control())
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
    }

    #[test]
    fn enforces_device_token_length_and_printable_content() {
        assert!(valid_device_token("0123456789abcdef"));
        assert!(!valid_device_token("too-short"));
        assert!(!valid_device_token("0123456789abcde\n"));
        assert!(!valid_device_token(&"x".repeat(513)));
    }

    #[test]
    fn conversation_idle_rotation_is_explicitly_opt_in() {
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
    fn validates_worker_owned_hermes_memory_scopes() {
        assert!(valid_hermes_session_key("agent:main:voice:room:kitchen"));
        assert!(!valid_hermes_session_key(""));
        assert!(!valid_hermes_session_key("   "));
        assert!(!valid_hermes_session_key(" room:kitchen"));
        assert!(!valid_hermes_session_key("room:kitchen "));
        assert!(!valid_hermes_session_key("room\nother"));
        assert!(!valid_hermes_session_key(&"x".repeat(257)));
    }
}
