use serde::Serialize;
use worker::{Response, Result as WorkerResult};

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub code: &'static str,
    pub message: &'static str,
}

#[derive(Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
}

impl ApiError {
    pub const fn new(status: u16, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            code,
            message,
        }
    }

    pub const fn bad_request(code: &'static str, message: &'static str) -> Self {
        Self::new(400, code, message)
    }

    pub const fn unauthorized() -> Self {
        Self::new(401, "unauthorized", "Authentication failed")
    }

    pub const fn payload_too_large() -> Self {
        Self::new(
            413,
            "audio_too_large",
            "Audio payload exceeds the configured limit",
        )
    }

    pub const fn unsupported_media_type() -> Self {
        Self::new(
            415,
            "unsupported_media_type",
            "Content-Type must be audio/wav",
        )
    }

    pub const fn configuration() -> Self {
        Self::new(
            503,
            "service_unavailable",
            "The voice gateway is not configured",
        )
    }

    pub fn upstream(service: &'static str) -> Self {
        match service {
            "stt" => Self::new(502, "stt_upstream_error", "Speech transcription failed"),
            "hermes" => Self::new(502, "hermes_upstream_error", "Hermes request failed"),
            "tts" => Self::new(502, "tts_upstream_error", "Speech synthesis failed"),
            _ => Self::new(502, "upstream_error", "An upstream service failed"),
        }
    }

    pub const fn internal() -> Self {
        Self::new(500, "internal_error", "The voice gateway failed")
    }

    pub fn into_response(self) -> WorkerResult<Response> {
        Response::from_json(&ErrorEnvelope {
            error: ErrorBody {
                code: self.code,
                message: self.message,
            },
        })
        .map(|response| response.with_status(self.status))
    }
}

pub type ApiResult<T> = std::result::Result<T, ApiError>;
