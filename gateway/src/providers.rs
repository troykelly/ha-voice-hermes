use std::time::Duration;

use futures_util::{future, pin_mut, StreamExt};
use js_sys::Uint8Array;
use serde_json::{json, Value};
use wasm_bindgen::JsValue;
use worker::{
    AbortController, Delay, Fetch, Headers, Method, Request, RequestInit, RequestRedirect, Response,
};

use crate::config::{Config, SttProvider};
use crate::error::{ApiError, ApiResult};
use crate::text::extract_hermes_response;

const MAX_TRANSCRIPT_BYTES: usize = 64 * 1024;
const MAX_STT_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const PROVIDER_CONNECT_TIMEOUT_SECONDS: u64 = 30;
const PROVIDER_BODY_IDLE_TIMEOUT_SECONDS: u64 = 30;

pub async fn transcribe(config: &Config, wav: &[u8]) -> ApiResult<String> {
    let endpoint = match config.stt_provider {
        SttProvider::ElevenLabs => api_endpoint(&config.stt_base_url, "speech-to-text"),
        SttProvider::OpenAiCompatible => api_endpoint(&config.stt_base_url, "audio/transcriptions"),
    };
    let boundary = multipart_boundary(wav);
    let mut fields = vec![(
        stt_model_field(config.stt_provider),
        config.stt_model.as_str(),
    )];
    if config.stt_provider == SttProvider::ElevenLabs {
        fields.push(("tag_audio_events", "false"));
        fields.push(("diarize", "false"));
        fields.push((
            "enable_logging",
            if config.elevenlabs_enable_logging {
                "true"
            } else {
                "false"
            },
        ));
    }
    if let Some(language_code) = config.stt_language_code.as_deref() {
        fields.push((stt_language_field(config.stt_provider), language_code));
    }
    let body = multipart_wav(&boundary, &fields, wav);

    let headers = Headers::new();
    headers
        .set(
            "Content-Type",
            &format!("multipart/form-data; boundary={boundary}"),
        )
        .map_err(|_| ApiError::internal())?;
    headers
        .set("Accept", "application/json")
        .map_err(|_| ApiError::internal())?;
    match config.stt_provider {
        SttProvider::ElevenLabs => headers
            .set("xi-api-key", &config.stt_api_key)
            .map_err(|_| ApiError::internal())?,
        SttProvider::OpenAiCompatible => headers
            .set("Authorization", &format!("Bearer {}", config.stt_api_key))
            .map_err(|_| ApiError::internal())?,
    }

    let mut response = send_bytes_request(&endpoint, headers, body, "stt").await?;
    let response_bytes =
        read_response_limited(&mut response, MAX_STT_RESPONSE_BYTES, "stt").await?;
    let payload: Value =
        serde_json::from_slice(&response_bytes).map_err(|_| ApiError::upstream("stt"))?;
    let transcript = payload
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty() && text.len() <= MAX_TRANSCRIPT_BYTES)
        .ok_or_else(|| ApiError::upstream("stt"))?;
    Ok(transcript.to_string())
}

pub async fn ask_hermes(config: &Config, device_id: &str, transcript: &str) -> ApiResult<String> {
    let endpoint = api_endpoint(&config.hermes_base_url, "responses");
    // Buffered v1 is a disabled-by-default diagnostic path. Keep it stateless
    // and non-stored so concurrent diagnostics cannot race a named chain and
    // can never collide with realtime v2's Durable-Object-owned response head.
    let body = json!({
        "model": config.hermes_model,
        "input": transcript,
        "instructions": config.hermes_voice_instructions,
        "store": false
    });

    let headers = Headers::new();
    headers
        .set("Content-Type", "application/json")
        .map_err(|_| ApiError::internal())?;
    headers
        .set("Accept", "application/json")
        .map_err(|_| ApiError::internal())?;
    headers
        .set(
            "Authorization",
            &format!("Bearer {}", config.hermes_api_key),
        )
        .map_err(|_| ApiError::internal())?;
    headers
        .set(
            "X-Hermes-Session-Key",
            &config.diagnostic_hermes_session_key(device_id),
        )
        .map_err(|_| ApiError::internal())?;
    if let (Some(client_id), Some(client_secret)) = (
        config.cf_access_client_id.as_deref(),
        config.cf_access_client_secret.as_deref(),
    ) {
        headers
            .set("CF-Access-Client-Id", client_id)
            .map_err(|_| ApiError::internal())?;
        headers
            .set("CF-Access-Client-Secret", client_secret)
            .map_err(|_| ApiError::internal())?;
    }

    let serialized = serde_json::to_string(&body).map_err(|_| ApiError::internal())?;
    let mut response = send_json_request(&endpoint, headers, serialized, "hermes").await?;
    let response_bytes =
        read_response_limited(&mut response, config.max_hermes_response_bytes, "hermes").await?;
    let payload: Value =
        serde_json::from_slice(&response_bytes).map_err(|_| ApiError::upstream("hermes"))?;
    extract_hermes_response(&payload).ok_or_else(|| ApiError::upstream("hermes"))
}
pub async fn synthesize(config: &Config, spoken_text: &str) -> ApiResult<Response> {
    let endpoint = format!(
        "{}?output_format=pcm_16000&enable_logging={}",
        api_endpoint(
            &config.elevenlabs_base_url,
            &format!("text-to-speech/{}/stream", config.elevenlabs_voice_id)
        ),
        config.elevenlabs_enable_logging
    );
    let body = serde_json::to_string(&json!({
        "text": spoken_text,
        "model_id": config.tts_model
    }))
    .map_err(|_| ApiError::internal())?;

    let headers = Headers::new();
    headers
        .set("Content-Type", "application/json")
        .map_err(|_| ApiError::internal())?;
    headers
        .set("Accept", "application/octet-stream")
        .map_err(|_| ApiError::internal())?;
    headers
        .set("xi-api-key", &config.elevenlabs_api_key)
        .map_err(|_| ApiError::internal())?;

    let mut upstream = send_json_request(&endpoint, headers, body, "tts").await?;
    // The diagnostic endpoint used to proxy this stream without any bound.
    // A compromised provider could therefore send arbitrary amounts of data.
    // Buffering this disabled-by-default path lets it enforce the same output-
    // duration contract as realtime before any audio reaches the caller.
    let maximum_audio_bytes = maximum_pcm_bytes(config.realtime_max_output_seconds);
    let audio = read_response_limited(&mut upstream, maximum_audio_bytes, "tts").await?;
    if audio.is_empty() || !audio.len().is_multiple_of(2) {
        return Err(ApiError::upstream("tts"));
    }
    let mut response = Response::from_bytes(audio).map_err(|_| ApiError::internal())?;
    response
        .headers_mut()
        .set(
            "Content-Type",
            "audio/pcm; rate=16000; channels=1; format=s16le",
        )
        .map_err(|_| ApiError::internal())?;
    response
        .headers_mut()
        .set("Cache-Control", "no-store")
        .map_err(|_| ApiError::internal())?;
    response
        .headers_mut()
        .set("X-Audio-Sample-Rate", "16000")
        .map_err(|_| ApiError::internal())?;
    response
        .headers_mut()
        .set("X-Audio-Channels", "1")
        .map_err(|_| ApiError::internal())?;
    response
        .headers_mut()
        .set("X-Audio-Bits-Per-Sample", "16")
        .map_err(|_| ApiError::internal())?;
    Ok(response)
}

fn maximum_pcm_bytes(maximum_seconds: u32) -> usize {
    usize::try_from(maximum_seconds)
        .unwrap_or(usize::MAX)
        .saturating_mul(16_000)
        .saturating_mul(2)
}

async fn send_bytes_request(
    endpoint: &str,
    headers: Headers,
    body: Vec<u8>,
    service: &'static str,
) -> ApiResult<Response> {
    let bytes = Uint8Array::from(body.as_slice());
    send_request(endpoint, headers, bytes.into(), service).await
}

async fn send_json_request(
    endpoint: &str,
    headers: Headers,
    body: String,
    service: &'static str,
) -> ApiResult<Response> {
    send_request(endpoint, headers, JsValue::from_str(&body), service).await
}

async fn send_request(
    endpoint: &str,
    headers: Headers,
    body: JsValue,
    service: &'static str,
) -> ApiResult<Response> {
    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        // Provider requests carry API keys and Access credentials. Never let
        // a 30x replay custom secret headers to a different origin.
        // Workers does not implement redirect="error". Manual returns the
        // 30x response without following it; the status check below rejects
        // it before credentials can be replayed to Location.
        .with_redirect(RequestRedirect::Manual)
        .with_body(Some(body));
    let request = Request::new_with_init(endpoint, &init).map_err(|_| ApiError::configuration())?;
    let controller = AbortController::default();
    let signal = controller.signal();
    let fetch_request = Fetch::Request(request);
    let fetch = fetch_request.send_with_signal(&signal);
    let timeout = Delay::from(Duration::from_secs(PROVIDER_CONNECT_TIMEOUT_SECONDS));
    pin_mut!(fetch, timeout);
    let response = match future::select(fetch, timeout).await {
        future::Either::Left((response, _)) => response.map_err(|_| ApiError::upstream(service))?,
        future::Either::Right(_) => {
            controller.abort_with_reason("provider connection timed out");
            return Err(ApiError::upstream(service));
        }
    };
    if !(200..300).contains(&response.status_code()) {
        return Err(ApiError::upstream(service));
    }
    Ok(response)
}

async fn read_response_limited(
    response: &mut Response,
    limit: usize,
    service: &'static str,
) -> ApiResult<Vec<u8>> {
    if let Some(content_length) = response
        .headers()
        .get("Content-Length")
        .map_err(|_| ApiError::upstream(service))?
    {
        let content_length = content_length
            .parse::<usize>()
            .map_err(|_| ApiError::upstream(service))?;
        if content_length > limit {
            return Err(ApiError::upstream(service));
        }
    }

    let mut stream = response.stream().map_err(|_| ApiError::upstream(service))?;
    let mut bytes = Vec::new();
    loop {
        let next = stream.next();
        let timeout = Delay::from(Duration::from_secs(PROVIDER_BODY_IDLE_TIMEOUT_SECONDS));
        pin_mut!(next, timeout);
        let chunk = match future::select(next, timeout).await {
            future::Either::Left((Some(chunk), _)) => {
                chunk.map_err(|_| ApiError::upstream(service))?
            }
            future::Either::Left((None, _)) => break,
            future::Either::Right(_) => return Err(ApiError::upstream(service)),
        };
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(ApiError::upstream(service));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn api_endpoint(base_url: &str, path_under_v1: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    if base_url.ends_with("/v1") {
        format!("{base_url}/{}", path_under_v1.trim_start_matches('/'))
    } else {
        format!("{base_url}/v1/{}", path_under_v1.trim_start_matches('/'))
    }
}

fn stt_model_field(provider: SttProvider) -> &'static str {
    match provider {
        SttProvider::ElevenLabs => "model_id",
        SttProvider::OpenAiCompatible => "model",
    }
}

fn stt_language_field(provider: SttProvider) -> &'static str {
    match provider {
        SttProvider::ElevenLabs => "language_code",
        SttProvider::OpenAiCompatible => "language",
    }
}

fn multipart_boundary(wav: &[u8]) -> String {
    let hash = wav.iter().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    });
    format!("----------------ha-voice-hermes-{hash:016x}")
}

fn multipart_wav(boundary: &str, fields: &[(&str, &str)], wav: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(wav.len() + 1_024);
    for (name, value) in fields {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: audio/wav\r\n\r\n");
    body.extend_from_slice(wav);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_v1_base_urls_without_duplication() {
        assert_eq!(
            api_endpoint("https://example.com/v1", "responses"),
            "https://example.com/v1/responses"
        );
        assert_eq!(
            api_endpoint("https://example.com", "/responses"),
            "https://example.com/v1/responses"
        );
    }

    #[test]
    fn multipart_contains_model_and_wav() {
        let wav = b"RIFF-test-WAVE";
        let body = multipart_wav("boundary", &[("model_id", "scribe_v2")], wav);
        assert!(body.windows(wav.len()).any(|window| window == wav));
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("name=\"model_id\"\r\n\r\nscribe_v2"));
        assert!(text.ends_with("--boundary--\r\n"));
    }

    #[test]
    fn selects_provider_specific_stt_fields() {
        assert_eq!(stt_model_field(SttProvider::ElevenLabs), "model_id");
        assert_eq!(stt_model_field(SttProvider::OpenAiCompatible), "model");
        assert_eq!(stt_language_field(SttProvider::ElevenLabs), "language_code");
        assert_eq!(
            stt_language_field(SttProvider::OpenAiCompatible),
            "language"
        );
    }

    #[test]
    fn voice_instructions_are_speech_specific() {
        assert!(crate::config::DEFAULT_VOICE_INSTRUCTIONS.contains("natural spoken answer"));
        assert!(crate::config::DEFAULT_VOICE_INSTRUCTIONS.contains("Do not use Markdown"));
        assert!(crate::config::DEFAULT_VOICE_INSTRUCTIONS.contains("raw URLs"));
    }

    #[test]
    fn diagnostic_tts_output_uses_the_realtime_pcm_duration_bound() {
        assert_eq!(maximum_pcm_bytes(1), 32_000);
        assert_eq!(maximum_pcm_bytes(30), 960_000);
    }
}
