mod config;
mod error;
mod phrase;
mod provider_events;
mod providers;
mod realtime;
mod realtime_protocol;
mod sse;
mod text;
mod wav;

use futures_util::StreamExt;
use serde::Serialize;
use worker::{event, Context, Env, Method, Request, Response, Result as WorkerResult};

use config::{valid_device_id, Config};
use error::{ApiError, ApiResult};
use text::prepare_for_tts_strict;
use wav::{validate_wav, WavError};

#[derive(Serialize)]
struct HealthResponse<'a> {
    status: &'a str,
    service: &'a str,
    version: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    stt_provider: Option<&'a str>,
    realtime: bool,
}

#[derive(Serialize)]
struct ReadinessResponse<'a> {
    status: &'a str,
}

#[event(fetch)]
pub async fn main(request: Request, env: Env, _context: Context) -> WorkerResult<Response> {
    let result = dispatch(request, &env).await;
    let mut response = match result {
        Ok(response) => response,
        Err(error) => {
            let status = error.status;
            let mut response = error.into_response()?;
            if status == 401 {
                response.headers_mut().set("WWW-Authenticate", "Bearer")?;
            }
            response
        }
    };
    // A WebSocket upgrade response returned by a subrequest owns immutable
    // handshake headers and cannot be cloned. Browser policy headers are not
    // meaningful after the protocol switches, so leave a 101 untouched.
    if response.status_code() != 101 {
        add_common_headers(&mut response)?;
    }
    Ok(response)
}

async fn dispatch(mut request: Request, env: &Env) -> ApiResult<Response> {
    let path = request.path();
    match (request.method(), path.as_str()) {
        (Method::Get, "/health") => health(env),
        (Method::Get, "/healthz") => readiness(env),
        (Method::Get, "/v2/realtime") => realtime::upgrade(request, env).await,
        (Method::Post, "/v1/voice") => voice(&mut request, env).await,
        (_, "/health") => method_not_allowed("GET"),
        (_, "/healthz") => method_not_allowed("GET"),
        (_, "/v1/voice") => match Config::from_env(env) {
            Ok(config) if config.diagnostic_v1_enabled => method_not_allowed("POST"),
            _ => Err(ApiError::new(404, "not_found", "Route not found")),
        },
        (_, "/v2/realtime") => method_not_allowed("GET"),
        _ => Err(ApiError::new(404, "not_found", "Route not found")),
    }
}

fn configured_runtime(env: &Env) -> ApiResult<Config> {
    Config::from_env(env).and_then(|config| {
        env.durable_object("VOICE_SESSIONS")
            .map(|_| config)
            .map_err(|_| ApiError::configuration())
    })
}

fn readiness(env: &Env) -> ApiResult<Response> {
    let (status, code) = if configured_runtime(env).is_ok() {
        ("ok", 200)
    } else {
        ("unavailable", 503)
    };
    Response::from_json(&ReadinessResponse { status })
        .map(|response| response.with_status(code))
        .map_err(|_| ApiError::internal())
}

fn health(env: &Env) -> ApiResult<Response> {
    match configured_runtime(env) {
        Ok(config) => Response::from_json(&HealthResponse {
            status: "ok",
            service: "ha-voice-hermes-gateway",
            version: env!("CARGO_PKG_VERSION"),
            stt_provider: Some(config.stt_provider.as_str()),
            realtime: true,
        })
        .map_err(|_| ApiError::internal()),
        Err(_) => Response::from_json(&HealthResponse {
            status: "degraded",
            service: "ha-voice-hermes-gateway",
            version: env!("CARGO_PKG_VERSION"),
            stt_provider: None,
            realtime: false,
        })
        .map(|response| response.with_status(503))
        .map_err(|_| ApiError::internal()),
    }
}

async fn voice(request: &mut Request, env: &Env) -> ApiResult<Response> {
    let config = Config::from_env(env)?;
    if !config.diagnostic_v1_enabled {
        return Err(ApiError::new(404, "not_found", "Route not found"));
    }
    require_wav_content_type(request)?;

    let device_id = request
        .headers()
        .get("X-Device-Id")
        .map_err(|_| ApiError::bad_request("invalid_device_id", "Device ID is invalid"))?
        .filter(|value| valid_device_id(value))
        .ok_or_else(|| ApiError::bad_request("invalid_device_id", "Device ID is invalid"))?;

    let supplied_token = bearer_token(request).ok_or_else(ApiError::unauthorized)?;
    if !config.authenticate(&device_id, &supplied_token) {
        return Err(ApiError::unauthorized());
    }

    reject_oversized_content_length(request, config.max_audio_bytes)?;
    let wav = read_body_limited(request, config.max_audio_bytes).await?;
    validate_wav(&wav, config.max_audio_seconds).map_err(map_wav_error)?;

    let transcript = providers::transcribe(&config, &wav).await?;
    let hermes_response = providers::ask_hermes(&config, &device_id, &transcript).await?;
    let spoken_text =
        prepare_for_tts_strict(&hermes_response).ok_or_else(|| ApiError::upstream("hermes"))?;
    providers::synthesize(&config, &spoken_text).await
}

fn require_wav_content_type(request: &Request) -> ApiResult<()> {
    let content_type = request
        .headers()
        .get("Content-Type")
        .map_err(|_| ApiError::unsupported_media_type())?
        .unwrap_or_default();
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if !matches!(
        media_type.as_str(),
        "audio/wav" | "audio/wave" | "audio/x-wav"
    ) {
        return Err(ApiError::unsupported_media_type());
    }
    Ok(())
}

fn bearer_token(request: &Request) -> Option<String> {
    request
        .headers()
        .get("Authorization")
        .ok()
        .flatten()
        .and_then(|header| {
            header
                .strip_prefix("Bearer ")
                .map(str::to_string)
                .filter(|token| !token.is_empty())
        })
}

fn reject_oversized_content_length(request: &Request, limit: usize) -> ApiResult<()> {
    let Some(value) = request.headers().get("Content-Length").map_err(|_| {
        ApiError::bad_request("invalid_content_length", "Content-Length is invalid")
    })?
    else {
        return Ok(());
    };
    let length = value.parse::<usize>().map_err(|_| {
        ApiError::bad_request("invalid_content_length", "Content-Length is invalid")
    })?;
    if length > limit {
        return Err(ApiError::payload_too_large());
    }
    Ok(())
}

async fn read_body_limited(request: &mut Request, limit: usize) -> ApiResult<Vec<u8>> {
    let mut stream = request
        .stream()
        .map_err(|_| ApiError::bad_request("invalid_audio", "Audio body is missing"))?;
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|_| ApiError::bad_request("invalid_audio", "Audio body could not be read"))?;
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(ApiError::payload_too_large());
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_audio",
            "Audio body is empty",
        ));
    }
    Ok(bytes)
}

fn map_wav_error(error: WavError) -> ApiError {
    match error {
        WavError::DurationExceeded => ApiError::new(
            413,
            "audio_too_long",
            "Audio duration exceeds the configured limit",
        ),
        _ => ApiError::bad_request("invalid_wav", "WAV format must be PCM16LE mono at 16000 Hz"),
    }
}

fn method_not_allowed(allowed: &str) -> ApiResult<Response> {
    let mut response = ApiError::new(405, "method_not_allowed", "Method not allowed")
        .into_response()
        .map_err(|_| ApiError::internal())?;
    response
        .headers_mut()
        .set("Allow", allowed)
        .map_err(|_| ApiError::internal())?;
    Ok(response)
}

fn add_common_headers(response: &mut Response) -> WorkerResult<()> {
    response
        .headers_mut()
        .set("X-Content-Type-Options", "nosniff")?;
    response
        .headers_mut()
        .set("Referrer-Policy", "no-referrer")?;
    response.headers_mut().set(
        "Permissions-Policy",
        "camera=(), geolocation=(), microphone=()",
    )?;
    if !response.headers().has("Cache-Control")? {
        response.headers_mut().set("Cache-Control", "no-store")?;
    }
    Ok(())
}
