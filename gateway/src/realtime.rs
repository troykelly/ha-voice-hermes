use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use futures_channel::mpsc::{channel, Receiver, Sender};
use futures_util::{future, pin_mut, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use wasm_bindgen::{JsCast, JsValue};
use worker::{
    durable_object, AbortController, Delay, DurableObject, Env, Error, Fetch, Headers, Method,
    Request, RequestInit, RequestRedirect, Response, ResponseBuilder, Result, State, Storage, Url,
    WebSocket, WebSocketIncomingMessage, WebSocketPair,
};

use crate::config::{valid_device_id, Config};
use crate::error::{ApiError, ApiResult};
use crate::phrase::{PhraseConfig, PhraseSegmenter};
use crate::provider_events::{BoundedWebSocketEvents, ProviderEvent};
use crate::realtime_protocol::{
    decode_audio_frame, decode_control_message, encode_audio_frame, encode_control_message,
    AudioFlags, AudioHeader, AudioKind, ControlDirection, ControlMessage, TurnId, AUDIO_FORMAT,
    AUDIO_SAMPLE_RATE, MAX_AUDIO_PAYLOAD_BYTES, MAX_CONTROL_MESSAGE_BYTES,
    PROTOCOL_VERSION as WIRE_PROTOCOL_VERSION,
};
use crate::sse::{SseDiscardPolicy, SseLimits, SseParser};
use crate::text::{StreamingTtsSanitizer, MAX_TTS_CHARS};

const DEVICE_PROTOCOL: &str = "hermes-voice.realtime.v2";
const DEVICE_ROUTE: &str = "/v2/realtime";
const INTERNAL_DEVICE_HEADER: &str = "X-Authenticated-Device-Id";
const INTERNAL_AUTH_FINGERPRINT_HEADER: &str = "X-Authenticated-Device-Credential";
const DEVICE_AUDIO_CHUNK_BYTES: usize = 2_048;
const MAX_PROVIDER_AUDIO_BYTES: usize = 128 * 1024;
const MAX_DEVICE_BUFFERED_BYTES: u32 = 256 * 1024;
const MAX_STT_BUFFERED_BYTES: u32 = 128 * 1024;
const INPUT_ACK_INTERVAL: u32 = 4;
const OUTPUT_WINDOW_FRAMES: u32 = 32;
const OUTPUT_ACK_TIMEOUT_SECONDS: u64 = 10;
const PROVIDER_EVENT_QUEUE_CAPACITY: usize = 8;
const TURN_TIMEOUT_SECONDS: u64 = 15 * 60;
const RECOVERY_TIMEOUT_SECONDS: u64 = 30;
const PROVIDER_CONNECT_TIMEOUT_SECONDS: u64 = 15;
const STT_SESSION_TIMEOUT_SECONDS: u64 = 15;
const STT_COMMIT_TIMEOUT_SECONDS: u64 = 20;
const HERMES_CONNECT_TIMEOUT_SECONDS: u64 = 30;
const STREAMING_OUTPUT_TIMEOUT_SECONDS: u64 = 10 * 60;
const TTS_KEEPALIVE_SECONDS: u64 = 15;
const DEADLINE_POLL_SECONDS: u64 = 1;
const MAX_STT_EVENT_BYTES: usize = 128 * 1024;
const MAX_TTS_EVENT_BYTES: usize = 256 * 1024;
const MAX_TRANSCRIPT_CHARS: usize = 16_000;
const MAX_TRANSCRIPT_BYTES: usize = 16 * 1024;
const MAX_HERMES_ERROR_BYTES: usize = 16 * 1024;
const MAX_HERMES_SSE_EVENT_BYTES: usize = 128 * 1024;
const MAX_HERMES_DISCARDED_EVENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_HERMES_STREAM_BYTES: usize = 8 * 1024 * 1024;
const USAGE_WINDOW_MS: u64 = 24 * 60 * 60 * 1_000;
const USAGE_MESSAGE_FLUSH_INTERVAL: u32 = 64;
const USAGE_AUDIO_FLUSH_BYTES: u64 = (DEVICE_AUDIO_CHUNK_BYTES * 4) as u64;

const STORAGE_LAST_SEEN_TURN: &str = "last_seen_turn_id";
// Read only during migration from pre-conversation-lifecycle deployments.
const STORAGE_LAST_COMPLETED_RESPONSE: &str = "last_completed_response_id";
const STORAGE_CONVERSATION_STATE: &str = "conversation_state";
const STORAGE_TURN_JOURNAL: &str = "turn_journal";
const STORAGE_USAGE_BUDGET: &str = "usage_budget";
const CONVERSATION_STATE_VERSION: u8 = 1;
const USAGE_BUDGET_VERSION: u8 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DeviceAttachment {
    device_id: String,
    hello_received: bool,
    /// SHA-256 of the credential accepted at the edge. It is not usable as a
    /// bearer, and lets a hibernating object revoke a socket after rotation.
    #[serde(default)]
    auth_fingerprint: String,
    #[serde(default)]
    message_count: u32,
    #[serde(default)]
    turn_attempt_count: u32,
    #[serde(default)]
    audio_bytes: u64,
    #[serde(default)]
    unpersisted_message_count: u32,
    #[serde(default)]
    unpersisted_audio_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct UsageBudget {
    version: u8,
    window_started_unix_ms: u64,
    message_count: u64,
    turn_attempt_count: u64,
    audio_bytes: u64,
}

#[derive(Clone, Copy, Default)]
struct UsageDelta {
    message_count: u64,
    turn_attempt_count: u64,
    audio_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct TurnJournal {
    turn_id: String,
    #[serde(default)]
    conversation_id: Option<String>,
    prior_response_id: Option<String>,
    inflight_response_id: Option<String>,
    #[serde(default)]
    idempotency_key: Option<String>,
    state: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct ConversationState {
    version: u8,
    conversation_id: String,
    #[serde(default)]
    binding: Option<ConversationBinding>,
    last_completed_response_id: Option<String>,
    hermes_session_id: Option<String>,
    last_activity_unix_ms: Option<f64>,
    last_reset_request_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ConversationBinding {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    fingerprint: String,
    // Legacy raw fields are accepted only long enough to rotate a pre-v0.3
    // chain. New writes contain only a one-way, non-bearer fingerprint.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    hermes_base_url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    hermes_model: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    hermes_session_key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    hermes_profile_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    binding_revision: String,
}

impl ConversationBinding {
    fn from_config(config: &Config, device_id: &str) -> Self {
        let hermes_session_key = config.hermes_session_key(device_id);
        let fields = [
            config.hermes_base_url.as_str(),
            config.hermes_model.as_str(),
            hermes_session_key.as_str(),
            config.hermes_profile_id.as_str(),
            config.hermes_binding_revision.as_str(),
        ];
        let mut hasher = Sha256::new();
        for field in fields {
            hasher.update((field.len() as u64).to_be_bytes());
            hasher.update(field.as_bytes());
        }
        let mut fingerprint = String::with_capacity(64);
        for byte in hasher.finalize() {
            use std::fmt::Write as _;
            let _ = write!(fingerprint, "{byte:02x}");
        }
        Self {
            fingerprint,
            hermes_base_url: String::new(),
            hermes_model: String::new(),
            hermes_session_key: String::new(),
            hermes_profile_id: String::new(),
            binding_revision: String::new(),
        }
    }
}

#[derive(Serialize)]
struct CompletionWrite {
    conversation_state: ConversationState,
    turn_journal: TurnJournal,
}

#[derive(Serialize)]
struct StartWrite {
    last_seen_turn_id: String,
    conversation_state: ConversationState,
    turn_journal: TurnJournal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TurnPhase {
    Recovering,
    ConnectingStt,
    StreamingInput,
    WaitingForTranscript,
    RunningHermes,
    StreamingOutput,
}

enum StartStorage {
    Duplicate,
    Ready {
        conversation_id: String,
        prior_response_id: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReconcileOutcome {
    Current,
    Stale,
    Ambiguous,
}

struct ActiveTurn {
    id: u64,
    id_hex: String,
    generation: u64,
    phase: TurnPhase,
    conversation_id: Option<String>,
    prior_response_id: Option<String>,
    next_input_seq: u32,
    input_samples: u32,
    max_input_samples: u32,
    last_input_ack: Option<u32>,
    short_input_frame_seen: bool,
    stt_pending: Vec<u8>,
    next_output_seq: u32,
    output_samples: u32,
    max_output_samples: u32,
    last_output_ack: Option<u32>,
    output_ack_tx: Option<Sender<u32>>,
    stt: Option<WebSocket>,
    tts: Option<WebSocket>,
    stt_connect_abort: Option<AbortController>,
    tts_connect_abort: Option<AbortController>,
    hermes_abort: Option<AbortController>,
    response_id: Option<String>,
    hermes_session_id: Option<String>,
    started_at_ms: f64,
    turn_deadline_ms: f64,
    phase_deadline_ms: f64,
}

/// Reframes arbitrarily-bounded provider PCM events into the fixed device
/// transport frames. Only `finish` may return a short frame, so a provider
/// event boundary can never masquerade as the end of device audio.
#[derive(Default)]
struct PlaybackReframer {
    pending: Vec<u8>,
}

impl PlaybackReframer {
    fn push(&mut self, mut audio: &[u8]) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();

        if !self.pending.is_empty() {
            let needed = DEVICE_AUDIO_CHUNK_BYTES - self.pending.len();
            let copied = needed.min(audio.len());
            self.pending.extend_from_slice(&audio[..copied]);
            audio = &audio[copied..];
            if self.pending.len() == DEVICE_AUDIO_CHUNK_BYTES {
                frames.push(std::mem::take(&mut self.pending));
            }
        }

        while audio.len() >= DEVICE_AUDIO_CHUNK_BYTES {
            frames.push(audio[..DEVICE_AUDIO_CHUNK_BYTES].to_vec());
            audio = &audio[DEVICE_AUDIO_CHUNK_BYTES..];
        }
        self.pending.extend_from_slice(audio);
        frames
    }

    fn finish(&mut self) -> std::result::Result<Option<Vec<u8>>, &'static str> {
        if self.pending.len() & 1 != 0 {
            return Err("TTS ended with an incomplete PCM sample");
        }
        Ok((!self.pending.is_empty()).then(|| std::mem::take(&mut self.pending)))
    }
}

#[derive(Default)]
struct Runtime {
    device_id: Option<String>,
    device: Option<WebSocket>,
    generation: u64,
    conversation_reset_in_progress: bool,
    turn: Option<ActiveTurn>,
}

pub async fn upgrade(request: Request, env: &Env) -> ApiResult<Response> {
    require_websocket_upgrade(&request)?;
    require_subprotocol(&request)?;
    let device_id = request
        .headers()
        .get("X-Device-Id")
        .map_err(|_| ApiError::bad_request("invalid_device_id", "Device ID is invalid"))?
        .filter(|value| valid_device_id(value))
        .ok_or_else(|| ApiError::bad_request("invalid_device_id", "Device ID is invalid"))?;
    let token = bearer_token(&request).ok_or_else(ApiError::unauthorized)?;
    let config = Config::from_env(env)?;
    let auth_fingerprint = config
        .authenticate_fingerprint(&device_id, &token)
        .ok_or_else(ApiError::unauthorized)?;
    drop(token);

    // Runtime-owned incoming Request headers are immutable in workers-rs.
    // clone_mut preserves the WebSocket upgrade metadata while giving the
    // edge handler a private request on which to attach the authenticated
    // identity passed to the Durable Object.
    let mut forwarded = request.clone_mut().map_err(|_| ApiError::internal())?;
    let forwarded_headers = forwarded.headers_mut().map_err(|_| ApiError::internal())?;
    // The Durable Object needs only a non-bearer credential epoch after edge
    // authentication. Do not carry the raw device token into its request.
    forwarded_headers
        .delete("Authorization")
        .map_err(|_| ApiError::internal())?;
    forwarded_headers
        .set(INTERNAL_DEVICE_HEADER, &device_id)
        .map_err(|_| ApiError::internal())?;
    forwarded_headers
        .set(INTERNAL_AUTH_FINGERPRINT_HEADER, &auth_fingerprint)
        .map_err(|_| ApiError::internal())?;
    let namespace = env
        .durable_object("VOICE_SESSIONS")
        .map_err(|_| ApiError::configuration())?;
    let stub = namespace
        .id_from_name(&device_id)
        .and_then(|id| id.get_stub())
        .map_err(|_| ApiError::internal())?;
    let response = stub
        .fetch_with_request(forwarded)
        .await
        .map_err(|_| ApiError::internal())?;
    match response.status_code() {
        101 => Ok(response),
        429 => Err(ApiError::new(
            429,
            "quota_exhausted",
            "Device usage quota exhausted",
        )),
        _ => Err(ApiError::internal()),
    }
}

fn require_websocket_upgrade(request: &Request) -> ApiResult<()> {
    let upgrade = request
        .headers()
        .get("Upgrade")
        .map_err(|_| ApiError::bad_request("invalid_upgrade", "WebSocket upgrade is required"))?
        .unwrap_or_default();
    if !upgrade.eq_ignore_ascii_case("websocket") {
        return Err(ApiError::bad_request(
            "invalid_upgrade",
            "WebSocket upgrade is required",
        ));
    }
    Ok(())
}

fn require_subprotocol(request: &Request) -> ApiResult<()> {
    let protocols = request
        .headers()
        .get("Sec-WebSocket-Protocol")
        .map_err(|_| ApiError::bad_request("invalid_protocol", "Realtime protocol is required"))?
        .unwrap_or_default();
    if !protocols
        .split(',')
        .any(|protocol| protocol.trim() == DEVICE_PROTOCOL)
    {
        return Err(ApiError::bad_request(
            "invalid_protocol",
            "Realtime protocol is required",
        ));
    }
    Ok(())
}

fn bearer_token(request: &Request) -> Option<String> {
    request
        .headers()
        .get("Authorization")
        .ok()
        .flatten()
        .and_then(|value| value.strip_prefix("Bearer ").map(str::to_owned))
        .filter(|value| !value.is_empty())
}

fn valid_auth_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn fresh_usage_budget(now_unix_ms: u64) -> UsageBudget {
    UsageBudget {
        version: USAGE_BUDGET_VERSION,
        window_started_unix_ms: now_unix_ms,
        message_count: 0,
        turn_attempt_count: 0,
        audio_bytes: 0,
    }
}

fn usage_limits(config: &Config) -> (u64, u64, u64) {
    (
        u64::from(config.max_messages_per_connection),
        u64::from(config.max_turns_per_connection),
        u64::from(config.max_connection_audio_seconds)
            .saturating_mul(u64::from(AUDIO_SAMPLE_RATE))
            .saturating_mul(2),
    )
}

fn usage_budget_exhausted(budget: &UsageBudget, config: &Config) -> bool {
    usage_budget_reached_limits(budget, usage_limits(config))
}

fn usage_budget_reached_limits(budget: &UsageBudget, limits: (u64, u64, u64)) -> bool {
    let (maximum_messages, maximum_turns, maximum_audio_bytes) = limits;
    budget.message_count >= maximum_messages
        || budget.turn_attempt_count >= maximum_turns
        || budget.audio_bytes >= maximum_audio_bytes
}

fn usage_budget_for_time(existing: UsageBudget, now_unix_ms: u64) -> Result<UsageBudget> {
    if existing.version != USAGE_BUDGET_VERSION || existing.window_started_unix_ms == 0 {
        return Err(Error::RustError("invalid durable usage budget".into()));
    }
    if now_unix_ms >= existing.window_started_unix_ms
        && now_unix_ms - existing.window_started_unix_ms >= USAGE_WINDOW_MS
    {
        Ok(fresh_usage_budget(now_unix_ms))
    } else {
        // A wall-clock rollback must never create a free abuse window.
        Ok(existing)
    }
}

async fn initialize_usage_budget(storage: &Storage) -> Result<()> {
    let now = unix_now_ms().max(1.0) as u64;
    match storage.get::<UsageBudget>(STORAGE_USAGE_BUDGET).await? {
        Some(existing) => {
            let current = usage_budget_for_time(existing.clone(), now)?;
            if current != existing {
                storage.put(STORAGE_USAGE_BUDGET, current).await?;
            }
        }
        None => {
            storage
                .put(STORAGE_USAGE_BUDGET, fresh_usage_budget(now))
                .await?;
        }
    }
    Ok(())
}

async fn durable_usage_budget_exhausted(storage: &Storage, config: &Config) -> Result<bool> {
    let now = unix_now_ms().max(1.0) as u64;
    let existing = storage
        .get::<UsageBudget>(STORAGE_USAGE_BUDGET)
        .await?
        .ok_or_else(|| Error::RustError("missing durable usage budget".into()))?;
    let current = usage_budget_for_time(existing.clone(), now)?;
    if current != existing {
        storage.put(STORAGE_USAGE_BUDGET, current.clone()).await?;
    }
    Ok(usage_budget_exhausted(&current, config))
}

async fn charge_usage_budget(
    storage: &Storage,
    config: &Config,
    delta: UsageDelta,
) -> Result<bool> {
    if delta.message_count == 0 && delta.turn_attempt_count == 0 && delta.audio_bytes == 0 {
        return Ok(false);
    }
    let now = unix_now_ms().max(1.0) as u64;
    let (maximum_messages, maximum_turns, maximum_audio_bytes) = usage_limits(config);
    let exceeded = Rc::new(Cell::new(false));
    let transaction_exceeded = exceeded.clone();
    storage
        .transaction(move |transaction| async move {
            let existing: UsageBudget = transaction.get(STORAGE_USAGE_BUDGET).await?;
            let mut usage = usage_budget_for_time(existing, now)?;
            usage.message_count = usage.message_count.saturating_add(delta.message_count);
            usage.turn_attempt_count = usage
                .turn_attempt_count
                .saturating_add(delta.turn_attempt_count);
            usage.audio_bytes = usage.audio_bytes.saturating_add(delta.audio_bytes);
            transaction_exceeded.set(
                usage.message_count > maximum_messages
                    || usage.turn_attempt_count > maximum_turns
                    || usage.audio_bytes > maximum_audio_bytes,
            );
            transaction.put(STORAGE_USAGE_BUDGET, usage).await
        })
        .await?;
    Ok(exceeded.get())
}

#[durable_object]
pub struct VoiceSession {
    state: State,
    env: Env,
    storage: Rc<Storage>,
    runtime: Rc<RefCell<Runtime>>,
}

impl DurableObject for VoiceSession {
    fn new(state: State, env: Env) -> Self {
        let storage = Rc::new(state.storage());
        Self {
            state,
            env,
            storage,
            runtime: Rc::new(RefCell::new(Runtime::default())),
        }
    }

    async fn fetch(&self, request: Request) -> Result<Response> {
        if request.path() != DEVICE_ROUTE {
            return Response::error("Not found", 404);
        }
        let device_id = request
            .headers()
            .get(INTERNAL_DEVICE_HEADER)?
            .filter(|value| valid_device_id(value))
            .ok_or_else(|| Error::RustError("missing authenticated device identity".into()))?;
        let auth_fingerprint = request
            .headers()
            .get(INTERNAL_AUTH_FINGERPRINT_HEADER)?
            .filter(|value| valid_auth_fingerprint(value))
            .ok_or_else(|| Error::RustError("missing authenticated credential epoch".into()))?;

        let config = Config::from_env(&self.env)
            .map_err(|_| Error::RustError("gateway configuration unavailable".into()))?;
        initialize_usage_budget(&self.storage).await?;

        cancel_active_turn(&self.runtime, "connection_replaced", false);
        let mut durable_quota_exceeded = false;
        for existing in self.state.get_websockets() {
            // Flush the old hibernating attachment before replacement. Close
            // callbacks normally do this too, but accounting here prevents a
            // rapid reconnect loop from depending on callback delivery to
            // persist its final small message/audio batch.
            if let Ok(Some(mut attachment)) = existing.deserialize_attachment::<DeviceAttachment>()
            {
                durable_quota_exceeded |= self
                    .flush_attachment_usage(&config, &mut attachment, false)
                    .await?;
                existing.serialize_attachment(&attachment)?;
            }
            let _ = existing.close(Some(4001), Some("connection replaced"));
        }
        durable_quota_exceeded |= durable_usage_budget_exhausted(&self.storage, &config).await?;
        if durable_quota_exceeded {
            return Response::error("Device usage quota exceeded", 429);
        }

        let pair = WebSocketPair::new()?;
        pair.server
            .as_ref()
            .set_binary_type(worker::web_sys::BinaryType::Arraybuffer);
        pair.server.serialize_attachment(&DeviceAttachment {
            device_id: device_id.clone(),
            hello_received: false,
            auth_fingerprint,
            message_count: 0,
            turn_attempt_count: 0,
            audio_bytes: 0,
            unpersisted_message_count: 0,
            unpersisted_audio_bytes: 0,
        })?;
        self.state.accept_web_socket(&pair.server);
        {
            let mut runtime = self.runtime.borrow_mut();
            runtime.device_id = Some(device_id);
            runtime.device = Some(pair.server.clone());
        }
        let mut response = ResponseBuilder::new()
            .with_status(101)
            .with_websocket(pair.client)
            .empty();
        response
            .headers_mut()
            .set("Sec-WebSocket-Protocol", DEVICE_PROTOCOL)?;
        Ok(response)
    }

    async fn websocket_message(
        &self,
        socket: WebSocket,
        message: WebSocketIncomingMessage,
    ) -> Result<()> {
        let mut attachment: DeviceAttachment = socket
            .deserialize_attachment()?
            .ok_or_else(|| Error::RustError("missing websocket attachment".into()))?;
        {
            let mut runtime = self.runtime.borrow_mut();
            // After hibernation the in-memory socket is absent and is restored
            // here. If a replaced socket delivers a delayed callback, never
            // let it overwrite the newer transport epoch.
            if runtime
                .device
                .as_ref()
                .is_some_and(|current| current != &socket)
            {
                return Ok(());
            }
            runtime.device_id = Some(attachment.device_id.clone());
            runtime.device = Some(socket.clone());
        }

        let config = match Config::from_env(&self.env) {
            Ok(config) => config,
            Err(_) => {
                send_error(&self.runtime, None, "configuration_error", true);
                let _ = socket.close(Some(1011), Some("gateway configuration changed"));
                return Ok(());
            }
        };
        if config
            .expected_token_fingerprint(&attachment.device_id)
            .as_deref()
            != Some(attachment.auth_fingerprint.as_str())
        {
            cancel_active_turn(&self.runtime, "authentication_failed", false);
            send_error(&self.runtime, None, "authentication_failed", true);
            let _ = socket.close(Some(4003), Some("credential revoked"));
            return Ok(());
        }
        attachment.message_count = attachment.message_count.saturating_add(1);
        attachment.unpersisted_message_count =
            attachment.unpersisted_message_count.saturating_add(1);
        if attachment.message_count > config.max_messages_per_connection {
            cancel_active_turn(&self.runtime, "connection_quota_exceeded", false);
            send_error(&self.runtime, None, "queue_overflow", true);
            let _ = socket.close(Some(4008), Some("connection message quota exceeded"));
            return Ok(());
        }
        // Keep unflushed durable-budget deltas in the hibernating attachment.
        // Malformed frames and abrupt closes therefore cannot normally avoid
        // accounting; small batches keep storage writes off the 64 ms audio
        // hot path.
        socket.serialize_attachment(&attachment)?;

        match message {
            WebSocketIncomingMessage::String(text) => {
                let control = match decode_control_message(&text, MAX_CONTROL_MESSAGE_BYTES) {
                    Ok(control) => control,
                    Err(_) => {
                        cancel_active_turn(&self.runtime, "protocol_error", false);
                        send_error(&self.runtime, None, "protocol_error", true);
                        let _ = socket.close(Some(1002), Some("invalid control"));
                        return Ok(());
                    }
                };
                if control
                    .validate_direction(ControlDirection::DeviceToGateway)
                    .is_err()
                {
                    cancel_active_turn(&self.runtime, "protocol_error", false);
                    send_error(
                        &self.runtime,
                        control.turn_id().map(TurnId::get),
                        "protocol_error",
                        true,
                    );
                    let _ = socket.close(Some(1002), Some("invalid control direction"));
                    return Ok(());
                }
                let is_hello = matches!(control, ControlMessage::Hello { .. });
                if !attachment.hello_received && !is_hello {
                    send_error(&self.runtime, None, "hello_required", true);
                    let _ = socket.close(Some(1002), Some("hello required"));
                    return Ok(());
                }
                let turn_attempt = matches!(&control, ControlMessage::TurnStart { .. });
                if turn_attempt {
                    attachment.turn_attempt_count = attachment.turn_attempt_count.saturating_add(1);
                    if attachment.turn_attempt_count > config.max_turns_per_connection {
                        cancel_active_turn(&self.runtime, "connection_quota_exceeded", false);
                        send_error(
                            &self.runtime,
                            control.turn_id().map(TurnId::get),
                            "queue_overflow",
                            true,
                        );
                        let _ = socket.close(Some(4008), Some("connection turn quota exceeded"));
                        return Ok(());
                    }
                }
                let flush_for_boundary = matches!(
                    &control,
                    ControlMessage::ConversationReset { .. }
                        | ControlMessage::TurnStart { .. }
                        | ControlMessage::TurnCommit { .. }
                        | ControlMessage::TurnCancel { .. }
                        | ControlMessage::Ping { .. }
                );
                if flush_for_boundary
                    || attachment.unpersisted_message_count >= USAGE_MESSAGE_FLUSH_INTERVAL
                {
                    let exceeded = match self
                        .flush_attachment_usage(&config, &mut attachment, turn_attempt)
                        .await
                    {
                        Ok(exceeded) => exceeded,
                        Err(_) => {
                            send_error(&self.runtime, None, "internal_error", true);
                            let _ = socket.close(Some(1011), Some("usage accounting unavailable"));
                            return Ok(());
                        }
                    };
                    socket.serialize_attachment(&attachment)?;
                    if exceeded {
                        cancel_active_turn(&self.runtime, "connection_quota_exceeded", false);
                        send_error(
                            &self.runtime,
                            control.turn_id().map(TurnId::get),
                            "queue_overflow",
                            true,
                        );
                        let _ = socket.close(Some(4008), Some("daily device quota exceeded"));
                        return Ok(());
                    }
                }
                socket.serialize_attachment(&attachment)?;
                self.handle_control(&socket, attachment, control).await
            }
            WebSocketIncomingMessage::Binary(bytes) => {
                if !attachment.hello_received {
                    send_error(&self.runtime, None, "hello_required", true);
                    let _ = socket.close(Some(1002), Some("hello required"));
                    return Ok(());
                }
                let payload_bytes = bytes
                    .len()
                    .saturating_sub(crate::realtime_protocol::AUDIO_HEADER_LEN);
                attachment.audio_bytes = attachment
                    .audio_bytes
                    .saturating_add(u64::try_from(payload_bytes).unwrap_or(u64::MAX));
                attachment.unpersisted_audio_bytes = attachment
                    .unpersisted_audio_bytes
                    .saturating_add(u64::try_from(payload_bytes).unwrap_or(u64::MAX));
                let maximum_audio_bytes = u64::from(config.max_connection_audio_seconds)
                    .saturating_mul(u64::from(AUDIO_SAMPLE_RATE))
                    .saturating_mul(2);
                if attachment.audio_bytes > maximum_audio_bytes {
                    cancel_active_turn(&self.runtime, "connection_quota_exceeded", false);
                    send_error(&self.runtime, None, "queue_overflow", true);
                    let _ = socket.close(Some(4008), Some("connection audio quota exceeded"));
                    return Ok(());
                }
                if attachment.unpersisted_message_count >= USAGE_MESSAGE_FLUSH_INTERVAL
                    || attachment.unpersisted_audio_bytes >= USAGE_AUDIO_FLUSH_BYTES
                    || payload_bytes < DEVICE_AUDIO_CHUNK_BYTES
                {
                    let exceeded = match self
                        .flush_attachment_usage(&config, &mut attachment, false)
                        .await
                    {
                        Ok(exceeded) => exceeded,
                        Err(_) => {
                            send_error(&self.runtime, None, "internal_error", true);
                            let _ = socket.close(Some(1011), Some("usage accounting unavailable"));
                            return Ok(());
                        }
                    };
                    if exceeded {
                        cancel_active_turn(&self.runtime, "connection_quota_exceeded", false);
                        send_error(&self.runtime, None, "queue_overflow", true);
                        let _ = socket.close(Some(4008), Some("daily device quota exceeded"));
                        return Ok(());
                    }
                }
                socket.serialize_attachment(&attachment)?;
                self.handle_audio(&bytes).await
            }
        }
    }

    async fn websocket_close(
        &self,
        socket: WebSocket,
        _code: usize,
        _reason: String,
        _was_clean: bool,
    ) -> Result<()> {
        self.flush_socket_usage(&socket).await;
        let is_current = self
            .runtime
            .borrow()
            .device
            .as_ref()
            .is_some_and(|current| current == &socket);
        if is_current {
            cancel_active_turn(&self.runtime, "device_disconnected", false);
            let mut runtime = self.runtime.borrow_mut();
            runtime.device = None;
            runtime.device_id = None;
        }
        Ok(())
    }

    async fn websocket_error(&self, socket: WebSocket, _error: Error) -> Result<()> {
        self.flush_socket_usage(&socket).await;
        let is_current = self
            .runtime
            .borrow()
            .device
            .as_ref()
            .is_some_and(|current| current == &socket);
        if is_current {
            cancel_active_turn(&self.runtime, "device_socket_error", false);
            let mut runtime = self.runtime.borrow_mut();
            runtime.device = None;
            runtime.device_id = None;
        }
        Ok(())
    }
}

impl VoiceSession {
    async fn flush_attachment_usage(
        &self,
        config: &Config,
        attachment: &mut DeviceAttachment,
        turn_attempt: bool,
    ) -> Result<bool> {
        let delta = UsageDelta {
            message_count: u64::from(attachment.unpersisted_message_count),
            turn_attempt_count: u64::from(turn_attempt),
            audio_bytes: attachment.unpersisted_audio_bytes,
        };
        let exceeded = charge_usage_budget(&self.storage, config, delta).await?;
        attachment.unpersisted_message_count = 0;
        attachment.unpersisted_audio_bytes = 0;
        Ok(exceeded)
    }

    async fn flush_socket_usage(&self, socket: &WebSocket) {
        let Ok(Some(mut attachment)) = socket.deserialize_attachment::<DeviceAttachment>() else {
            return;
        };
        if attachment.unpersisted_message_count == 0 && attachment.unpersisted_audio_bytes == 0 {
            return;
        }
        let Ok(config) = Config::from_env(&self.env) else {
            return;
        };
        if self
            .flush_attachment_usage(&config, &mut attachment, false)
            .await
            .is_ok()
        {
            let _ = socket.serialize_attachment(&attachment);
        }
    }

    async fn handle_control(
        &self,
        socket: &WebSocket,
        mut attachment: DeviceAttachment,
        control: ControlMessage,
    ) -> Result<()> {
        match control {
            ControlMessage::Hello { .. } => {
                if attachment.hello_received {
                    cancel_active_turn(&self.runtime, "protocol_error", false);
                    send_error(&self.runtime, None, "protocol_error", true);
                    let _ = socket.close(Some(1002), Some("duplicate hello"));
                    return Ok(());
                }
                attachment.hello_received = true;
                socket.serialize_attachment(&attachment)?;
                let conversation = match load_conversation_state(&self.storage).await {
                    Ok(conversation) => conversation,
                    Err(_) => {
                        send_error(&self.runtime, None, "conversation_unavailable", true);
                        let _ = socket.close(Some(1011), Some("conversation unavailable"));
                        return Ok(());
                    }
                };
                send_ready(socket, &conversation.conversation_id)?;
                Ok(())
            }
            ControlMessage::ConversationReset { request_id, .. } => {
                self.reset_conversation(&attachment.device_id, &request_id)
                    .await
            }
            ControlMessage::TurnStart { turn_id, .. } => {
                self.start_turn(&attachment.device_id, turn_id.get()).await
            }
            ControlMessage::TurnCommit {
                turn_id, last_seq, ..
            } => self.commit_turn(turn_id.get(), last_seq),
            ControlMessage::TurnCancel { turn_id, .. } => {
                let active_matches = self
                    .runtime
                    .borrow()
                    .turn
                    .as_ref()
                    .is_some_and(|turn| turn.id == turn_id.get());
                if active_matches {
                    cancel_active_turn(&self.runtime, "device_cancelled", true);
                }
                Ok(())
            }
            ControlMessage::OutputAck {
                turn_id,
                seq: sequence,
                ..
            } => {
                let turn_id = turn_id.get();
                let sender = if let Some(turn) = self.runtime.borrow_mut().turn.as_mut() {
                    if turn.id == turn_id
                        && sequence < turn.next_output_seq
                        && turn
                            .last_output_ack
                            .is_none_or(|previous| sequence > previous)
                    {
                        turn.last_output_ack = Some(sequence);
                        turn.output_ack_tx.clone()
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some(mut sender) = sender {
                    // Cumulative ACK state above is authoritative. This
                    // capacity-one channel is only a coalesced wake-up for a
                    // blocked TTS reader, so dropping a redundant wake-up is
                    // safe and prevents a device from growing object memory.
                    let _ = sender.try_send(sequence);
                }
                Ok(())
            }
            ControlMessage::Ping { .. } => {
                send_device_control(
                    &self.runtime,
                    &ControlMessage::Pong {
                        v: WIRE_PROTOCOL_VERSION,
                    },
                );
                Ok(())
            }
            ControlMessage::Pong { .. } => Ok(()),
            _ => unreachable!("control direction was validated before dispatch"),
        }
    }

    async fn reset_conversation(&self, device_id: &str, request_id: &str) -> Result<()> {
        let config = match Config::from_env(&self.env) {
            Ok(config) => config,
            Err(_) => {
                send_error(&self.runtime, None, "conversation_reset_failed", false);
                return Ok(());
            }
        };
        let binding = ConversationBinding::from_config(&config, device_id);
        {
            let mut runtime = self.runtime.borrow_mut();
            if runtime.turn.is_some() || runtime.conversation_reset_in_progress {
                drop(runtime);
                send_error(&self.runtime, None, "conversation_busy", false);
                return Ok(());
            }
            // Set this before the first await so a turn.start callback cannot
            // race the durable reset if the runtime ever releases an input gate.
            runtime.conversation_reset_in_progress = true;
        }

        let result = reset_conversation_storage(&self.storage, request_id, binding).await;
        self.runtime.borrow_mut().conversation_reset_in_progress = false;
        match result {
            Ok(conversation) => {
                send_device_control(
                    &self.runtime,
                    &ControlMessage::ConversationResetDone {
                        v: WIRE_PROTOCOL_VERSION,
                        request_id: request_id.to_string(),
                        conversation_id: conversation.conversation_id,
                    },
                );
            }
            Err(_) => send_error(&self.runtime, None, "conversation_reset_failed", false),
        }
        Ok(())
    }

    async fn start_turn(&self, device_id: &str, turn_id: u64) -> Result<()> {
        let config = Config::from_env(&self.env)
            .map_err(|_| Error::RustError("gateway configuration is invalid".into()))?;
        let turn_hex = format_turn_id(turn_id);
        let generation = {
            let mut runtime = self.runtime.borrow_mut();
            if runtime.conversation_reset_in_progress {
                drop(runtime);
                send_error(&self.runtime, Some(turn_id), "conversation_busy", false);
                return Ok(());
            }
            if runtime.turn.is_some() {
                drop(runtime);
                send_error(&self.runtime, Some(turn_id), "turn_in_progress", false);
                return Ok(());
            }
            runtime.generation = runtime.generation.wrapping_add(1);
            let generation = runtime.generation;
            let started_at_ms = monotonic_now_ms();
            runtime.turn = Some(ActiveTurn {
                id: turn_id,
                id_hex: turn_hex,
                generation,
                phase: TurnPhase::Recovering,
                conversation_id: None,
                prior_response_id: None,
                next_input_seq: 0,
                input_samples: 0,
                max_input_samples: config
                    .realtime_max_audio_seconds
                    .saturating_mul(AUDIO_SAMPLE_RATE),
                last_input_ack: None,
                short_input_frame_seen: false,
                stt_pending: Vec::with_capacity(DEVICE_AUDIO_CHUNK_BYTES * 2),
                next_output_seq: 0,
                output_samples: 0,
                max_output_samples: config
                    .realtime_max_output_seconds
                    .saturating_mul(AUDIO_SAMPLE_RATE),
                last_output_ack: None,
                output_ack_tx: None,
                stt: None,
                tts: None,
                stt_connect_abort: None,
                tts_connect_abort: None,
                hermes_abort: None,
                response_id: None,
                hermes_session_id: None,
                started_at_ms,
                turn_deadline_ms: started_at_ms + seconds_ms(TURN_TIMEOUT_SECONDS),
                phase_deadline_ms: started_at_ms + seconds_ms(RECOVERY_TIMEOUT_SECONDS),
            });
            generation
        };
        mark_stage(&self.runtime, turn_id, generation, "turn_reserved");

        let prepared_conversation =
            match prepare_conversation_for_turn(&config, device_id, &self.storage).await {
                Ok(conversation) => conversation,
                Err(_) => {
                    fail_turn_if_current(
                        &self.runtime,
                        turn_id,
                        generation,
                        "recovery_unavailable",
                    );
                    return Ok(());
                }
            };

        match reconcile_inflight_response(
            &config,
            &self.storage,
            &self.runtime,
            turn_id,
            generation,
            &prepared_conversation,
        )
        .await
        {
            Ok(ReconcileOutcome::Current) => {}
            Ok(ReconcileOutcome::Stale) => return Ok(()),
            Ok(ReconcileOutcome::Ambiguous) => {
                fail_turn_if_current(&self.runtime, turn_id, generation, "conversation_ambiguous");
                return Ok(());
            }
            Err(_) => {
                fail_turn_if_current(&self.runtime, turn_id, generation, "recovery_unavailable");
                return Ok(());
            }
        }

        let storage = match initialize_turn_storage(&self.storage, &format_turn_id(turn_id)).await {
            Ok(storage) => storage,
            Err(_) => {
                fail_turn_if_current(&self.runtime, turn_id, generation, "recovery_unavailable");
                return Ok(());
            }
        };
        if current_generation(&self.runtime, turn_id) != Some(generation) {
            return Ok(());
        }
        let prior_response_id = match storage {
            StartStorage::Duplicate => {
                fail_turn_if_current(&self.runtime, turn_id, generation, "duplicate_turn");
                return Ok(());
            }
            StartStorage::Ready {
                conversation_id,
                prior_response_id,
            } => {
                let mut runtime = self.runtime.borrow_mut();
                let Some(turn) = runtime
                    .turn
                    .as_mut()
                    .filter(|turn| turn.id == turn_id && turn.generation == generation)
                else {
                    return Ok(());
                };
                turn.conversation_id = Some(conversation_id);
                prior_response_id
            }
        };

        let stt_signal = {
            let controller = AbortController::default();
            let signal = controller.signal();
            let mut runtime = self.runtime.borrow_mut();
            let Some(turn) = runtime
                .turn
                .as_mut()
                .filter(|turn| turn.id == turn_id && turn.generation == generation)
            else {
                return Ok(());
            };
            turn.prior_response_id = prior_response_id;
            set_turn_phase(
                turn,
                TurnPhase::ConnectingStt,
                PROVIDER_CONNECT_TIMEOUT_SECONDS,
            );
            turn.stt_connect_abort = Some(controller);
            signal
        };
        mark_stage(&self.runtime, turn_id, generation, "turn_started");

        let stt_result = {
            let stt_future = connect_stt(&config, &stt_signal);
            let stt_timeout = Delay::from(remaining_turn_time(&self.runtime, turn_id, generation)?);
            pin_mut!(stt_future, stt_timeout);
            match future::select(stt_future, stt_timeout).await {
                future::Either::Left((result, _)) => result,
                future::Either::Right(_) => {
                    Err(Error::RustError("STT connection timed out".into()))
                }
            }
        };
        let stt = match stt_result {
            Ok(socket) => socket,
            Err(_) => {
                fail_turn_if_current(&self.runtime, turn_id, generation, "stt_connect_failed");
                return Ok(());
            }
        };
        {
            let mut runtime = self.runtime.borrow_mut();
            let Some(turn) = runtime
                .turn
                .as_mut()
                .filter(|turn| turn.id == turn_id && turn.generation == generation)
            else {
                let _ = stt.close(Some(1000), Some("stale turn"));
                return Ok(());
            };
            turn.stt_connect_abort = None;
            turn.stt = Some(stt.clone());
            set_turn_phase(turn, TurnPhase::ConnectingStt, STT_SESSION_TIMEOUT_SECONDS);
        }
        mark_stage(&self.runtime, turn_id, generation, "stt_connected");

        let runtime = self.runtime.clone();
        let storage = self.storage.clone();
        let device_id = device_id.to_string();
        self.state.wait_until(async move {
            if listen_stt(
                runtime.clone(),
                storage,
                config,
                device_id,
                turn_id,
                generation,
                stt,
            )
            .await
            .is_err()
            {
                fail_turn_if_current(&runtime, turn_id, generation, "stt_stream_failed");
            }
        });
        Ok(())
    }

    fn commit_turn(&self, turn_id: u64, last_seq: u32) -> Result<()> {
        let stt_result: std::result::Result<(Option<WebSocket>, Vec<u8>, u64), &'static str> = {
            let mut runtime = self.runtime.borrow_mut();
            match runtime.turn.as_mut().filter(|turn| turn.id == turn_id) {
                None => Err("stale_turn"),
                Some(turn)
                    if !matches!(turn.phase, TurnPhase::StreamingInput)
                        || turn.next_input_seq == 0 =>
                {
                    Err("turn_not_ready")
                }
                Some(turn) if Some(last_seq) != turn.next_input_seq.checked_sub(1) => {
                    Err("input_sequence_mismatch")
                }
                Some(turn) => {
                    set_turn_phase(
                        turn,
                        TurnPhase::WaitingForTranscript,
                        STT_COMMIT_TIMEOUT_SECONDS,
                    );
                    Ok((
                        turn.stt.clone(),
                        std::mem::take(&mut turn.stt_pending),
                        turn.generation,
                    ))
                }
            }
        };
        let (stt, pending, generation) = match stt_result {
            Ok(result) => result,
            Err(code) => {
                if let Some(generation) = current_generation(&self.runtime, turn_id) {
                    fail_turn_if_current(&self.runtime, turn_id, generation, code);
                } else {
                    send_error(&self.runtime, Some(turn_id), code, false);
                }
                return Ok(());
            }
        };
        if let Some(stt) = stt {
            if stt
                .send(&json!({
                "message_type": "input_audio_chunk",
                "audio_base_64": BASE64.encode(pending),
                "commit": true,
                "sample_rate": 16000
                }))
                .is_err()
            {
                fail_turn_if_current(&self.runtime, turn_id, generation, "stt_send_failed");
                return Ok(());
            }
            if !send_device_control(
                &self.runtime,
                &ControlMessage::InputAck {
                    v: WIRE_PROTOCOL_VERSION,
                    turn_id: TurnId::new(turn_id),
                    seq: last_seq,
                },
            ) {
                fail_turn_if_current(&self.runtime, turn_id, generation, "device_send_failed");
                return Ok(());
            }
            if let Some(turn) = self.runtime.borrow_mut().turn.as_mut() {
                if turn.id == turn_id {
                    turn.last_input_ack = Some(last_seq);
                }
            }
            mark_stage(&self.runtime, turn_id, generation, "input_committed");
        }
        Ok(())
    }

    async fn handle_audio(&self, bytes: &[u8]) -> Result<()> {
        let frame = match decode_audio_frame(bytes, MAX_AUDIO_PAYLOAD_BYTES) {
            Ok(frame) if frame.header.kind == AudioKind::MicrophonePcm => frame,
            Ok(_) => {
                fail_active_turn(&self.runtime, "invalid_audio_direction");
                return Ok(());
            }
            Err(_) => {
                fail_active_turn(&self.runtime, "invalid_audio_frame");
                return Ok(());
            }
        };
        let frame_turn_id = frame.header.turn_id.get();
        if frame.header.flags.discontinuity() {
            let generation = current_generation(&self.runtime, frame_turn_id).unwrap_or_default();
            fail_turn_if_current(
                &self.runtime,
                frame_turn_id,
                generation,
                "audio_discontinuity",
            );
            return Ok(());
        }
        if frame.header.flags.end_of_utterance() {
            if let Some(generation) = current_generation(&self.runtime, frame_turn_id) {
                mark_stage(&self.runtime, frame_turn_id, generation, "input_end_hint");
            }
        }
        enum InputAction {
            Forward {
                stt: Option<WebSocket>,
                ack: Option<u32>,
                turn_id: u64,
                audio: Option<Vec<u8>>,
            },
            Reject(&'static str),
            Fail(&'static str, u64),
        }
        let action = {
            let mut runtime = self.runtime.borrow_mut();
            match runtime
                .turn
                .as_mut()
                .filter(|turn| turn.id == frame_turn_id)
            {
                None => InputAction::Reject("stale_audio"),
                Some(turn) if turn.phase != TurnPhase::StreamingInput => {
                    InputAction::Reject("turn_not_ready")
                }
                Some(turn)
                    if frame.header.sequence != turn.next_input_seq
                        || frame.header.first_sample != turn.input_samples =>
                {
                    InputAction::Fail("input_sequence_mismatch", turn.generation)
                }
                Some(turn) if turn.short_input_frame_seen => {
                    InputAction::Fail("invalid_audio_frame", turn.generation)
                }
                Some(turn) => {
                    if !input_credit_available(turn.next_input_seq, turn.last_input_ack) {
                        InputAction::Fail("input_backpressure", turn.generation)
                    } else {
                        let samples = u32::try_from(frame.pcm.len() / 2)
                            .map_err(|_| Error::RustError("audio frame too large".into()))?;
                        let next_samples = turn.input_samples.saturating_add(samples);
                        if next_samples > turn.max_input_samples {
                            InputAction::Fail("audio_too_long", turn.generation)
                        } else {
                            if frame.pcm.len() < DEVICE_AUDIO_CHUNK_BYTES {
                                // Firmware emits at most one short frame when
                                // capture drains. No audio may follow it.
                                turn.short_input_frame_seen = true;
                            }
                            let stt = turn.stt.clone();
                            let ack = if frame.header.sequence % INPUT_ACK_INTERVAL
                                == INPUT_ACK_INTERVAL - 1
                            {
                                Some(frame.header.sequence)
                            } else {
                                None
                            };
                            turn.next_input_seq += 1;
                            turn.input_samples = next_samples;
                            turn.stt_pending.extend_from_slice(frame.pcm);
                            let audio = (turn.stt_pending.len() >= DEVICE_AUDIO_CHUNK_BYTES * 2)
                                .then(|| std::mem::take(&mut turn.stt_pending));
                            InputAction::Forward {
                                stt,
                                ack,
                                turn_id: turn.id,
                                audio,
                            }
                        }
                    }
                }
            }
        };
        let (stt, ack, turn_id, audio) = match action {
            InputAction::Forward {
                stt,
                ack,
                turn_id,
                audio,
            } => (stt, ack, turn_id, audio),
            InputAction::Reject(code) => {
                if let Some(generation) = current_generation(&self.runtime, frame_turn_id) {
                    fail_turn_if_current(&self.runtime, frame_turn_id, generation, code);
                } else {
                    send_error(&self.runtime, Some(frame_turn_id), code, false);
                }
                return Ok(());
            }
            InputAction::Fail(code, generation) => {
                fail_turn_if_current(&self.runtime, frame_turn_id, generation, code);
                return Ok(());
            }
        };
        let Some(stt) = stt else {
            if let Some(generation) = current_generation(&self.runtime, turn_id) {
                fail_turn_if_current(&self.runtime, turn_id, generation, "stt_unavailable");
            }
            return Ok(());
        };
        if let Some(audio) = audio {
            if stt.as_ref().buffered_amount() > MAX_STT_BUFFERED_BYTES {
                let generation = current_generation(&self.runtime, turn_id).unwrap_or_default();
                fail_turn_if_current(&self.runtime, turn_id, generation, "input_backpressure");
                return Ok(());
            }
            if stt
                .send(&json!({
                "message_type": "input_audio_chunk",
                "audio_base_64": BASE64.encode(audio),
                "commit": false,
                "sample_rate": 16000
                }))
                .is_err()
            {
                let generation = current_generation(&self.runtime, turn_id).unwrap_or_default();
                fail_turn_if_current(&self.runtime, turn_id, generation, "stt_send_failed");
                return Ok(());
            }
        }
        if frame.header.sequence == 0 {
            if let Some(generation) = current_generation(&self.runtime, turn_id) {
                mark_stage(&self.runtime, turn_id, generation, "input_first_frame");
            }
        }
        if let Some(sequence) = ack {
            if !send_device_control(
                &self.runtime,
                &ControlMessage::InputAck {
                    v: WIRE_PROTOCOL_VERSION,
                    turn_id: TurnId::new(turn_id),
                    seq: sequence,
                },
            ) {
                if let Some(generation) = current_generation(&self.runtime, turn_id) {
                    fail_turn_if_current(&self.runtime, turn_id, generation, "device_send_failed");
                }
                return Ok(());
            }
            if let Some(turn) = self.runtime.borrow_mut().turn.as_mut() {
                if turn.id == turn_id {
                    turn.last_input_ack = Some(sequence);
                }
            }
        }
        Ok(())
    }
}

fn input_credit_available(next_sequence: u32, last_ack: Option<u32>) -> bool {
    let acknowledged = last_ack.map_or(0, |sequence| sequence.saturating_add(1));
    next_sequence.saturating_sub(acknowledged) < 32
}

fn send_ready(socket: &WebSocket, conversation_id: &str) -> Result<()> {
    let encoded = encode_control_message(
        &ControlMessage::Ready {
            v: WIRE_PROTOCOL_VERSION,
            input_window: Some(32),
            output_window: Some(OUTPUT_WINDOW_FRAMES),
            frame_ms: Some(64),
            input_format: Some(AUDIO_FORMAT.to_string()),
            output_format: Some(AUDIO_FORMAT.to_string()),
            conversation_id: Some(conversation_id.to_string()),
        },
        MAX_CONTROL_MESSAGE_BYTES,
    )
    .map_err(|error| Error::RustError(error.to_string()))?;
    socket.send_with_str(encoded)
}

fn format_turn_id(turn_id: u64) -> String {
    TurnId::new(turn_id).to_string()
}

fn random_conversation_id() -> Result<String> {
    let global = js_sys::global();
    let crypto = js_sys::Reflect::get(&global, &JsValue::from_str("crypto"))
        .map_err(|_| Error::RustError("Web Crypto is unavailable".into()))?;
    let random_uuid = js_sys::Reflect::get(&crypto, &JsValue::from_str("randomUUID"))
        .map_err(|_| Error::RustError("crypto.randomUUID is unavailable".into()))?
        .dyn_into::<js_sys::Function>()
        .map_err(|_| Error::RustError("crypto.randomUUID is unavailable".into()))?;
    let value = random_uuid
        .call0(&crypto)
        .map_err(|_| Error::RustError("conversation ID generation failed".into()))?
        .as_string()
        .ok_or_else(|| Error::RustError("conversation ID generation failed".into()))?;
    if !valid_conversation_id(&value) {
        return Err(Error::RustError(
            "generated conversation ID is invalid".into(),
        ));
    }
    Ok(value)
}

fn valid_conversation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_reset_request_id(value: &str) -> bool {
    value.len() == 16
        && value != "0000000000000000"
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_hermes_session_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.bytes().any(|byte| byte.is_ascii_control())
}

fn valid_conversation_binding(binding: &ConversationBinding) -> bool {
    let fingerprint_only = binding.fingerprint.len() == 64
        && binding
            .fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && binding.hermes_base_url.is_empty()
        && binding.hermes_model.is_empty()
        && binding.hermes_session_key.is_empty()
        && binding.hermes_profile_id.is_empty()
        && binding.binding_revision.is_empty();
    if fingerprint_only {
        return true;
    }
    if !binding.fingerprint.is_empty() {
        return false;
    }

    // Accept the pre-v0.3 representation only for one migration read. It will
    // compare unequal to the current fingerprint, rotate the conversation,
    // and never be persisted again.
    let valid_url = Url::parse(&binding.hermes_base_url).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    });
    valid_url
        && binding.hermes_base_url.len() <= 2_048
        && !binding
            .hermes_base_url
            .bytes()
            .any(|byte| byte.is_ascii_control())
        && !binding.hermes_model.is_empty()
        && binding.hermes_model.len() <= 160
        && !binding
            .hermes_model
            .bytes()
            .any(|byte| byte.is_ascii_control())
        && !binding.hermes_session_key.is_empty()
        && binding.hermes_session_key == binding.hermes_session_key.trim()
        && binding.hermes_session_key.len() <= 256
        && !binding
            .hermes_session_key
            .bytes()
            .any(|byte| byte.is_ascii_control())
        && (binding.hermes_profile_id.is_empty()
            || (binding.hermes_profile_id.len() <= 160
                && binding.hermes_profile_id.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric()
                        || matches!(byte, b'-' | b'_' | b'.' | b'/' | b':' | b'@')
                })))
        && (binding.binding_revision.is_empty()
            || (binding.binding_revision.len() <= 160
                && binding.binding_revision.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric()
                        || matches!(byte, b'-' | b'_' | b'.' | b'/' | b':' | b'@')
                })))
}

fn validate_conversation_state(state: &ConversationState) -> Result<()> {
    if state.version != CONVERSATION_STATE_VERSION
        || !valid_conversation_id(&state.conversation_id)
        || state
            .binding
            .as_ref()
            .is_some_and(|binding| !valid_conversation_binding(binding))
        || state
            .last_completed_response_id
            .as_deref()
            .is_some_and(|value| !valid_response_id(value))
        || state
            .hermes_session_id
            .as_deref()
            .is_some_and(|value| !valid_hermes_session_id(value))
        || state
            .last_activity_unix_ms
            .is_some_and(|value| !value.is_finite() || value < 0.0)
        || state
            .last_reset_request_id
            .as_deref()
            .is_some_and(|value| !valid_reset_request_id(value))
    {
        return Err(Error::RustError(
            "persisted conversation state is invalid".into(),
        ));
    }
    Ok(())
}

fn fresh_conversation_state(
    conversation_id: String,
    last_reset_request_id: Option<String>,
    binding: Option<ConversationBinding>,
) -> ConversationState {
    ConversationState {
        version: CONVERSATION_STATE_VERSION,
        conversation_id,
        binding,
        last_completed_response_id: None,
        hermes_session_id: None,
        last_activity_unix_ms: None,
        last_reset_request_id,
    }
}

async fn persist_conversation_state(
    storage: &Storage,
    conversation: &ConversationState,
) -> Result<()> {
    validate_conversation_state(conversation)?;
    let conversation = conversation.clone();
    storage
        .transaction(move |transaction| async move {
            transaction
                .put(STORAGE_CONVERSATION_STATE, conversation)
                .await?;
            transaction.delete(STORAGE_LAST_COMPLETED_RESPONSE).await?;
            Ok(())
        })
        .await
}

async fn load_conversation_state(storage: &Storage) -> Result<ConversationState> {
    if let Some(conversation) = storage
        .get::<ConversationState>(STORAGE_CONVERSATION_STATE)
        .await?
    {
        validate_conversation_state(&conversation)?;
        return Ok(conversation);
    }

    let legacy_response_id: Option<String> = storage.get(STORAGE_LAST_COMPLETED_RESPONSE).await?;
    if legacy_response_id
        .as_deref()
        .is_some_and(|value| !valid_response_id(value))
    {
        return Err(Error::RustError(
            "persisted legacy response ID is invalid".into(),
        ));
    }
    let mut conversation = fresh_conversation_state(random_conversation_id()?, None, None);
    conversation.last_completed_response_id = legacy_response_id;
    persist_conversation_state(storage, &conversation).await?;
    Ok(conversation)
}

async fn reset_conversation_storage(
    storage: &Storage,
    request_id: &str,
    binding: ConversationBinding,
) -> Result<ConversationState> {
    if !valid_reset_request_id(request_id) {
        return Err(Error::RustError("reset request ID is invalid".into()));
    }
    let current = load_conversation_state(storage).await?;
    if current.last_reset_request_id.as_deref() == Some(request_id) {
        return Ok(current);
    }
    let conversation = fresh_conversation_state(
        random_conversation_id()?,
        Some(request_id.to_string()),
        Some(binding),
    );
    persist_conversation_state(storage, &conversation).await?;
    Ok(conversation)
}

async fn expire_conversation_storage(
    storage: &Storage,
    expected_conversation_id: &str,
) -> Result<ConversationState> {
    let current = load_conversation_state(storage).await?;
    if current.conversation_id != expected_conversation_id {
        return Err(Error::RustError(
            "conversation changed before expiry handling".into(),
        ));
    }
    let conversation =
        fresh_conversation_state(random_conversation_id()?, None, current.binding.clone());
    persist_conversation_state(storage, &conversation).await?;
    Ok(conversation)
}

fn unix_now_ms() -> f64 {
    js_sys::Date::now()
}

fn conversation_idle_expired(
    conversation: &ConversationState,
    idle_seconds: Option<u64>,
    now_unix_ms: f64,
) -> bool {
    let (Some(idle_seconds), Some(last_activity)) =
        (idle_seconds, conversation.last_activity_unix_ms)
    else {
        return false;
    };
    now_unix_ms.is_finite()
        && now_unix_ms >= last_activity
        && now_unix_ms - last_activity >= seconds_ms(idle_seconds)
}

fn journal_belongs_to_conversation(
    journal: &TurnJournal,
    conversation: &ConversationState,
) -> bool {
    journal.conversation_id.as_deref() == Some(conversation.conversation_id.as_str())
        || (journal.conversation_id.is_none()
            && conversation.last_reset_request_id.is_none()
            && journal.prior_response_id == conversation.last_completed_response_id)
}

async fn prepare_conversation_for_turn(
    config: &Config,
    device_id: &str,
    storage: &Storage,
) -> Result<ConversationState> {
    let mut current = load_conversation_state(storage).await?;
    let binding = ConversationBinding::from_config(config, device_id);
    if current
        .binding
        .as_ref()
        .is_some_and(|stored| stored != &binding)
    {
        let conversation = fresh_conversation_state(random_conversation_id()?, None, Some(binding));
        persist_conversation_state(storage, &conversation).await?;
        worker::console_log!("voice_conversation event=rotated reason=binding_changed");
        return Ok(conversation);
    }
    if current.binding.is_none() {
        current.binding = Some(binding.clone());
        persist_conversation_state(storage, &current).await?;
    }
    if !conversation_idle_expired(&current, config.conversation_idle_seconds, unix_now_ms()) {
        return Ok(current);
    }
    let conversation = fresh_conversation_state(random_conversation_id()?, None, Some(binding));
    persist_conversation_state(storage, &conversation).await?;
    worker::console_log!("voice_conversation event=rotated reason=idle");
    Ok(conversation)
}

async fn initialize_turn_storage(storage: &Storage, turn_hex: &str) -> Result<StartStorage> {
    let last_seen: Option<String> = storage.get(STORAGE_LAST_SEEN_TURN).await?;
    if last_seen.as_deref() == Some(turn_hex) {
        return Ok(StartStorage::Duplicate);
    }
    let mut conversation = load_conversation_state(storage).await?;
    let prior_response_id = conversation.last_completed_response_id.clone();
    // This is the authoritative user-interaction timestamp. The optional idle
    // policy measures inactivity between accepted turns, including turns that
    // later fail or are cancelled, rather than only successful completions.
    conversation.last_activity_unix_ms = Some(unix_now_ms());
    validate_conversation_state(&conversation)?;
    storage
        .put_multiple(StartWrite {
            last_seen_turn_id: turn_hex.to_string(),
            conversation_state: conversation.clone(),
            turn_journal: TurnJournal {
                turn_id: turn_hex.to_string(),
                conversation_id: Some(conversation.conversation_id.clone()),
                prior_response_id: prior_response_id.clone(),
                inflight_response_id: None,
                idempotency_key: None,
                state: "capturing".into(),
            },
        })
        .await?;
    Ok(StartStorage::Ready {
        conversation_id: conversation.conversation_id,
        prior_response_id,
    })
}

async fn write_current_turn_journal(
    storage: &Storage,
    runtime: &Rc<RefCell<Runtime>>,
    turn_id: u64,
    generation: u64,
    journal: TurnJournal,
) -> Result<()> {
    if !current_turn_matches(runtime, turn_id, generation) {
        return Err(Error::RustError("stale turn journal write".into()));
    }
    let current: Option<TurnJournal> = storage.get(STORAGE_TURN_JOURNAL).await?;
    if !current_turn_matches(runtime, turn_id, generation)
        || current.as_ref().map(|journal| journal.turn_id.as_str())
            != Some(format_turn_id(turn_id).as_str())
    {
        return Err(Error::RustError("turn journal ownership changed".into()));
    }
    storage.put(STORAGE_TURN_JOURNAL, journal).await
}

async fn promote_current_response(
    storage: &Storage,
    runtime: &Rc<RefCell<Runtime>>,
    turn_id: u64,
    generation: u64,
    completed_response_id: &str,
) -> Result<()> {
    let (conversation_id, hermes_session_id) = runtime
        .borrow()
        .turn
        .as_ref()
        .filter(|turn| turn.id == turn_id && turn.generation == generation)
        .and_then(|turn| {
            turn.conversation_id
                .clone()
                .map(|conversation_id| (conversation_id, turn.hermes_session_id.clone()))
        })
        .ok_or_else(|| Error::RustError("stale response promotion".into()))?;
    let journal: TurnJournal = storage
        .get(STORAGE_TURN_JOURNAL)
        .await?
        .ok_or_else(|| Error::RustError("missing turn journal".into()))?;
    if !current_turn_matches(runtime, turn_id, generation)
        || journal.turn_id != format_turn_id(turn_id)
        || journal.conversation_id.as_deref() != Some(conversation_id.as_str())
        || journal.inflight_response_id.as_deref() != Some(completed_response_id)
        || journal.state != "hermes_in_progress"
    {
        return Err(Error::RustError(
            "turn journal did not own Hermes completion".into(),
        ));
    }
    let mut conversation = load_conversation_state(storage).await?;
    if !current_turn_matches(runtime, turn_id, generation)
        || conversation.conversation_id != conversation_id
    {
        return Err(Error::RustError(
            "conversation changed during response promotion".into(),
        ));
    }
    conversation.last_completed_response_id = Some(completed_response_id.to_string());
    // Absence is authoritative too; do not retain stale metadata from an
    // upstream deployment that stopped emitting the continuity header.
    conversation.hermes_session_id = hermes_session_id;
    conversation.last_activity_unix_ms = Some(unix_now_ms());
    validate_conversation_state(&conversation)?;
    storage
        .put_multiple(CompletionWrite {
            conversation_state: conversation,
            turn_journal: TurnJournal {
                state: "completed".into(),
                ..journal
            },
        })
        .await
}

fn seconds_ms(seconds: u64) -> f64 {
    Duration::from_secs(seconds).as_secs_f64() * 1_000.0
}

fn set_turn_phase(turn: &mut ActiveTurn, phase: TurnPhase, timeout_seconds: u64) {
    turn.phase = phase;
    turn.phase_deadline_ms = monotonic_now_ms() + seconds_ms(timeout_seconds);
}

fn remaining_turn_time(
    runtime: &Rc<RefCell<Runtime>>,
    turn_id: u64,
    generation: u64,
) -> Result<Duration> {
    let now = monotonic_now_ms();
    let deadline = runtime
        .borrow()
        .turn
        .as_ref()
        .filter(|turn| turn.id == turn_id && turn.generation == generation)
        .map(|turn| turn.turn_deadline_ms.min(turn.phase_deadline_ms))
        .ok_or_else(|| Error::RustError("stale turn deadline".into()))?;
    if deadline <= now {
        return Err(Error::RustError("turn phase timed out".into()));
    }
    Ok(Duration::from_secs_f64((deadline - now) / 1_000.0))
}

fn current_turn_matches(runtime: &Rc<RefCell<Runtime>>, turn_id: u64, generation: u64) -> bool {
    current_generation(runtime, turn_id) == Some(generation)
}

fn send_device_control(runtime: &Rc<RefCell<Runtime>>, control: &ControlMessage) -> bool {
    let _message_type = control.message_type();
    if control
        .validate_direction(ControlDirection::GatewayToDevice)
        .is_err()
    {
        return false;
    }
    let Ok(encoded) = encode_control_message(control, MAX_CONTROL_MESSAGE_BYTES) else {
        return false;
    };
    if let Some(device) = runtime.borrow().device.as_ref() {
        return device.send_with_str(encoded).is_ok();
    }
    false
}

fn send_error(
    runtime: &Rc<RefCell<Runtime>>,
    turn_id: Option<u64>,
    code: &'static str,
    fatal: bool,
) {
    let code = public_error_code(code);
    send_device_control(
        runtime,
        &ControlMessage::Error {
            v: WIRE_PROTOCOL_VERSION,
            turn_id: turn_id.map(TurnId::new),
            code: code.to_string(),
            message: None,
            fatal: Some(fatal),
        },
    );
}

fn public_error_code(code: &'static str) -> &'static str {
    match code {
        "protocol_error"
        | "unsupported_version"
        | "invalid_frame"
        | "sequence_error"
        | "turn_conflict"
        | "unknown_turn"
        | "queue_overflow"
        | "turn_timeout"
        | "conversation_busy"
        | "conversation_reset_failed"
        | "conversation_unavailable"
        | "conversation_expired"
        | "conversation_ambiguous"
        | "stt_failed"
        | "empty_transcript"
        | "hermes_failed"
        | "tts_failed"
        | "cancelled"
        | "configuration_error"
        | "authentication_failed"
        | "internal_error" => code,
        "invalid_control" | "invalid_direction" | "hello_required" | "duplicate_hello" => {
            "protocol_error"
        }
        "invalid_audio_frame" | "invalid_audio_direction" => "invalid_frame",
        "input_sequence_mismatch" | "audio_discontinuity" => "sequence_error",
        "duplicate_turn" | "turn_in_progress" => "turn_conflict",
        "stale_turn" | "stale_audio" | "turn_not_ready" => "unknown_turn",
        "input_backpressure" | "connection_quota_exceeded" => "queue_overflow",
        "audio_too_long" => "turn_timeout",
        "recovery_unavailable" => "conversation_unavailable",
        "stt_connect_failed"
        | "stt_send_failed"
        | "stt_stream_failed"
        | "stt_unavailable"
        | "stt_unexpected_commit" => "stt_failed",
        "hermes_configuration_failed" | "hermes_connect_failed" | "hermes_busy" => "hermes_failed",
        "provider_connect_timeout" => "turn_timeout",
        "tts_connect_failed" => "tts_failed",
        "device_cancelled"
        | "connection_replaced"
        | "device_disconnected"
        | "device_socket_error" => "cancelled",
        _ => "internal_error",
    }
}

fn current_generation(runtime: &Rc<RefCell<Runtime>>, turn_id: u64) -> Option<u64> {
    runtime
        .borrow()
        .turn
        .as_ref()
        .filter(|turn| turn.id == turn_id)
        .map(|turn| turn.generation)
}

fn cancel_active_turn(runtime: &Rc<RefCell<Runtime>>, reason: &'static str, notify: bool) {
    let active = runtime
        .borrow()
        .turn
        .as_ref()
        .map(|turn| (turn.id, turn.generation));
    if let Some((turn_id, generation)) = active {
        mark_stage(
            runtime,
            turn_id,
            generation,
            if notify {
                "turn_cancelled"
            } else {
                "turn_aborted"
            },
        );
    }
    let turn = runtime.borrow_mut().turn.take();
    let Some(mut turn) = turn else {
        return;
    };
    if let Some(controller) = turn.stt_connect_abort.take() {
        controller.abort_with_reason(reason);
    }
    if let Some(controller) = turn.tts_connect_abort.take() {
        controller.abort_with_reason(reason);
    }
    if let Some(tts) = turn.tts.take() {
        let _ = tts.send(&json!({
            "context_id": turn.id_hex,
            "close_context": true
        }));
        let _ = tts.close(Some(1000), Some("turn cancelled"));
    }
    if let Some(stt) = turn.stt.take() {
        let _ = stt.close(Some(1000), Some("turn cancelled"));
    }
    if let Some(controller) = turn.hermes_abort.take() {
        controller.abort_with_reason(reason);
    }
    if notify {
        send_device_control(
            runtime,
            &ControlMessage::TurnDone {
                v: WIRE_PROTOCOL_VERSION,
                turn_id: TurnId::new(turn.id),
                cancelled: Some(true),
                reason: Some(reason.to_string()),
            },
        );
    }
}

fn fail_turn_if_current(
    runtime: &Rc<RefCell<Runtime>>,
    turn_id: u64,
    generation: u64,
    code: &'static str,
) {
    let is_current = runtime
        .borrow()
        .turn
        .as_ref()
        .is_some_and(|turn| turn.id == turn_id && turn.generation == generation);
    if is_current {
        mark_stage(runtime, turn_id, generation, "turn_failed");
        send_error(runtime, Some(turn_id), code, false);
        cancel_active_turn(runtime, code, false);
    }
}

fn fail_active_turn(runtime: &Rc<RefCell<Runtime>>, code: &'static str) {
    let active = runtime
        .borrow()
        .turn
        .as_ref()
        .map(|turn| (turn.id, turn.generation));
    if let Some((turn_id, generation)) = active {
        fail_turn_if_current(runtime, turn_id, generation, code);
    } else {
        send_error(runtime, None, code, false);
    }
}

fn mark_stage(runtime: &Rc<RefCell<Runtime>>, turn_id: u64, generation: u64, stage: &'static str) {
    let elapsed = runtime
        .borrow()
        .turn
        .as_ref()
        .filter(|turn| turn.id == turn_id && turn.generation == generation)
        .map(|turn| (monotonic_now_ms() - turn.started_at_ms).max(0.0) as u64);
    if let Some(elapsed_ms) = elapsed {
        worker::console_log!(
            "voice_metric turn={} stage={} elapsed_ms={}",
            format_turn_id(turn_id),
            stage,
            elapsed_ms
        );
    }
}

fn monotonic_now_ms() -> f64 {
    let global = js_sys::global();
    let Ok(performance) = js_sys::Reflect::get(&global, &JsValue::from_str("performance")) else {
        return 0.0;
    };
    let Ok(now) = js_sys::Reflect::get(&performance, &JsValue::from_str("now")) else {
        return 0.0;
    };
    let Ok(now) = now.dyn_into::<js_sys::Function>() else {
        return 0.0;
    };
    now.call0(&performance)
        .ok()
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0)
}

async fn connect_stt(config: &Config, signal: &worker::AbortSignal) -> Result<WebSocket> {
    let mut url = api_endpoint_url(&config.elevenlabs_base_url, "speech-to-text/realtime")?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("model_id", &config.realtime_stt_model);
        query.append_pair("audio_format", "pcm_16000");
        query.append_pair("commit_strategy", "manual");
        query.append_pair("include_timestamps", "false");
        query.append_pair(
            "enable_logging",
            if config.elevenlabs_enable_logging {
                "true"
            } else {
                "false"
            },
        );
        if let Some(language) = config.stt_language_code.as_deref() {
            query.append_pair("language_code", language);
        }
    }
    connect_provider_websocket(url, &config.elevenlabs_api_key, signal).await
}

async fn connect_tts(config: &Config, signal: &worker::AbortSignal) -> Result<WebSocket> {
    let path = format!(
        "text-to-speech/{}/multi-stream-input",
        config.elevenlabs_voice_id
    );
    let mut url = api_endpoint_url(&config.elevenlabs_base_url, &path)?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("model_id", &config.tts_model);
        query.append_pair("output_format", "pcm_16000");
        query.append_pair("auto_mode", "true");
        query.append_pair("sync_alignment", "false");
        query.append_pair("inactivity_timeout", "180");
        query.append_pair(
            "enable_logging",
            if config.elevenlabs_enable_logging {
                "true"
            } else {
                "false"
            },
        );
    }
    connect_provider_websocket(url, &config.elevenlabs_api_key, signal).await
}

async fn connect_provider_websocket(
    url: Url,
    api_key: &str,
    signal: &worker::AbortSignal,
) -> Result<WebSocket> {
    let headers = Headers::new();
    headers.set("Upgrade", "websocket")?;
    headers.set("xi-api-key", api_key)?;
    let mut init = RequestInit::new();
    init.with_method(Method::Get)
        .with_headers(headers)
        // Cloudflare forwards custom secret headers across followed redirects.
        // Provider authentication must never leave the configured origin.
        // workerd supports follow/manual but not error. Manual surfaces a 30x
        // response; requiring 101 below rejects it without following Location.
        .with_redirect(RequestRedirect::Manual);
    let request = Request::new_with_init(url.as_str(), &init)?;
    let fetch = Fetch::Request(request);
    let response = fetch.send_with_signal(signal).await?;
    if response.status_code() != 101 {
        return Err(Error::RustError(
            "provider rejected websocket upgrade".into(),
        ));
    }
    response
        .websocket()
        .ok_or_else(|| Error::RustError("provider did not return a websocket".into()))
}

fn api_endpoint_url(base_url: &str, path_under_v1: &str) -> Result<Url> {
    let base = base_url.trim_end_matches('/');
    let endpoint = if base.ends_with("/v1") {
        format!("{base}/{}", path_under_v1.trim_start_matches('/'))
    } else {
        format!("{base}/v1/{}", path_under_v1.trim_start_matches('/'))
    };
    Url::parse(&endpoint).map_err(Error::from)
}

async fn listen_stt(
    runtime: Rc<RefCell<Runtime>>,
    storage: Rc<Storage>,
    config: Config,
    device_id: String,
    turn_id: u64,
    generation: u64,
    stt: WebSocket,
) -> Result<()> {
    let mut events =
        BoundedWebSocketEvents::new(&stt, MAX_STT_EVENT_BYTES, PROVIDER_EVENT_QUEUE_CAPACITY)?;
    stt.accept()?;
    while let Some(event) =
        next_provider_event_before_deadline(&mut events, &runtime, turn_id, generation).await?
    {
        if current_generation(&runtime, turn_id) != Some(generation) {
            return Ok(());
        }
        match event {
            ProviderEvent::Text(text) => {
                let value: Value = serde_json::from_str(&text)?;
                match value
                    .get("message_type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                {
                    "session_started" => {
                        let became_ready = {
                            let mut state = runtime.borrow_mut();
                            state
                                .turn
                                .as_mut()
                                .filter(|turn| turn.id == turn_id && turn.generation == generation)
                                .is_some_and(|turn| {
                                    if turn.phase == TurnPhase::ConnectingStt {
                                        set_turn_phase(
                                            turn,
                                            TurnPhase::StreamingInput,
                                            u64::from(config.realtime_max_audio_seconds)
                                                .saturating_add(STT_SESSION_TIMEOUT_SECONDS),
                                        );
                                        true
                                    } else {
                                        false
                                    }
                                })
                        };
                        if became_ready {
                            mark_stage(&runtime, turn_id, generation, "stt_session_ready");
                            if !send_device_control(
                                &runtime,
                                &ControlMessage::TurnReady {
                                    v: WIRE_PROTOCOL_VERSION,
                                    turn_id: TurnId::new(turn_id),
                                },
                            ) {
                                return Err(Error::RustError("device rejected turn.ready".into()));
                            }
                        }
                    }
                    // Partials are mutable telemetry, not agent input. The
                    // firmware has no transcript UI and its inbound queue is
                    // intentionally small, so omit them instead of allowing a
                    // provider burst to crowd out authoritative controls.
                    "partial_transcript" => {}
                    "committed_transcript" => {
                        let transcript = bounded_text(
                            &value,
                            "text",
                            MAX_TRANSCRIPT_CHARS,
                            MAX_TRANSCRIPT_BYTES,
                        )
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .ok_or_else(|| Error::RustError("STT returned an empty transcript".into()))?
                        .to_string();
                        let valid_phase = {
                            let mut state = runtime.borrow_mut();
                            state
                                .turn
                                .as_mut()
                                .filter(|turn| turn.id == turn_id && turn.generation == generation)
                                .is_some_and(|turn| {
                                    if turn.phase == TurnPhase::WaitingForTranscript {
                                        set_turn_phase(
                                            turn,
                                            TurnPhase::RunningHermes,
                                            HERMES_CONNECT_TIMEOUT_SECONDS,
                                        );
                                        turn.stt = None;
                                        true
                                    } else {
                                        false
                                    }
                                })
                        };
                        if !valid_phase {
                            return Err(Error::RustError(
                                "STT committed outside the commit phase".into(),
                            ));
                        }
                        let _ = stt.close(Some(1000), Some("transcript committed"));
                        mark_stage(&runtime, turn_id, generation, "stt_final");
                        // The firmware has no transcript UI. Do not send user
                        // speech back over the device control plane, where a
                        // long UTF-8 transcript could exceed the 8 KiB frame
                        // bound and where it adds unnecessary PII exposure.
                        return run_agent_pipeline(
                            runtime, storage, config, device_id, turn_id, generation, transcript,
                        )
                        .await;
                    }
                    "auth_error"
                    | "quota_exceeded"
                    | "transcriber_error"
                    | "input_error"
                    | "commit_throttled"
                    | "unaccepted_terms"
                    | "rate_limited"
                    | "queue_overflow"
                    | "resource_exhausted"
                    | "session_time_limit_exceeded"
                    | "chunk_size_exceeded"
                    | "insufficient_audio_activity"
                    | "error" => {
                        return Err(Error::RustError("STT provider returned an error".into()));
                    }
                    _ => {}
                }
            }
            ProviderEvent::Close => {
                return Err(Error::RustError("STT websocket closed early".into()));
            }
            ProviderEvent::Error => {
                return Err(Error::RustError("STT websocket reported an error".into()));
            }
        }
    }
    Err(Error::RustError("STT websocket ended early".into()))
}

async fn next_provider_event_before_deadline(
    events: &mut BoundedWebSocketEvents,
    runtime: &Rc<RefCell<Runtime>>,
    turn_id: u64,
    generation: u64,
) -> Result<Option<ProviderEvent>> {
    loop {
        let remaining = remaining_turn_time(runtime, turn_id, generation)?;
        let event = events.next();
        let timeout = Delay::from(remaining.min(Duration::from_secs(DEADLINE_POLL_SECONDS)));
        pin_mut!(event, timeout);
        match future::select(event, timeout).await {
            future::Either::Left((event, _)) => return event,
            future::Either::Right(_) => {
                // Another callback can move the turn to a shorter phase while
                // this provider read is pending (notably turn.commit). Poll
                // the absolute deadline so the older timer cannot mask it.
                remaining_turn_time(runtime, turn_id, generation)?;
            }
        }
    }
}

fn bounded_text<'a>(
    value: &'a Value,
    key: &str,
    max_chars: usize,
    max_bytes: usize,
) -> Option<&'a str> {
    let text = value.get(key)?.as_str()?;
    (text.len() <= max_bytes && text.chars().count() <= max_chars).then_some(text)
}

async fn run_agent_pipeline(
    runtime: Rc<RefCell<Runtime>>,
    storage: Rc<Storage>,
    config: Config,
    device_id: String,
    turn_id: u64,
    generation: u64,
    transcript: String,
) -> Result<()> {
    if current_generation(&runtime, turn_id) != Some(generation) {
        return Ok(());
    }
    let (conversation_id, prior_response_id) = runtime
        .borrow()
        .turn
        .as_ref()
        .filter(|turn| turn.id == turn_id && turn.generation == generation)
        .and_then(|turn| {
            turn.conversation_id
                .clone()
                .map(|conversation_id| (conversation_id, turn.prior_response_id.clone()))
        })
        .ok_or_else(|| Error::RustError("turn has no conversation".into()))?;
    let idempotency_key = hermes_idempotency_key(&conversation_id, turn_id);
    write_current_turn_journal(
        &storage,
        &runtime,
        turn_id,
        generation,
        TurnJournal {
            turn_id: format_turn_id(turn_id),
            conversation_id: Some(conversation_id),
            prior_response_id: prior_response_id.clone(),
            inflight_response_id: None,
            idempotency_key: Some(idempotency_key.clone()),
            state: "starting_hermes".into(),
        },
    )
    .await?;
    if !current_turn_matches(&runtime, turn_id, generation) {
        return Ok(());
    }
    mark_stage(&runtime, turn_id, generation, "hermes_start");

    let tts_signal = {
        let controller = AbortController::default();
        let signal = controller.signal();
        let mut state = runtime.borrow_mut();
        let turn = state
            .turn
            .as_mut()
            .filter(|turn| turn.id == turn_id && turn.generation == generation)
            .ok_or_else(|| Error::RustError("stale turn before TTS connection".into()))?;
        turn.tts_connect_abort = Some(controller);
        signal
    };
    let tts_future = connect_tts(&config, &tts_signal);
    let hermes_future = open_hermes_stream(
        &config,
        &device_id,
        &transcript,
        prior_response_id.as_deref(),
        &runtime,
        turn_id,
        generation,
    );
    let providers = future::join(tts_future, hermes_future);
    let timeout = Delay::from(remaining_turn_time(&runtime, turn_id, generation)?);
    pin_mut!(providers, timeout);
    let (tts_result, hermes_result) = match future::select(providers, timeout).await {
        future::Either::Left((results, _)) => results,
        future::Either::Right(_) => {
            fail_turn_if_current(&runtime, turn_id, generation, "provider_connect_timeout");
            return Ok(());
        }
    };
    if let Some(turn) = runtime.borrow_mut().turn.as_mut() {
        if turn.id == turn_id && turn.generation == generation {
            turn.tts_connect_abort = None;
        }
    }

    let tts = match tts_result {
        Ok(tts) => tts,
        Err(_) => {
            fail_turn_if_current(&runtime, turn_id, generation, "tts_connect_failed");
            return Ok(());
        }
    };
    let hermes = match hermes_result {
        Ok(response) => response,
        Err(code) => {
            if code == "conversation_expired" {
                let conversation_id = runtime
                    .borrow()
                    .turn
                    .as_ref()
                    .filter(|turn| turn.id == turn_id && turn.generation == generation)
                    .and_then(|turn| turn.conversation_id.clone());
                let expiry = match conversation_id {
                    Some(conversation_id) => {
                        expire_conversation_storage(&storage, &conversation_id).await
                    }
                    None => Err(Error::RustError("turn has no conversation".into())),
                };
                if expiry.is_err() {
                    let _ = tts.close(Some(1000), Some("conversation recovery unavailable"));
                    fail_turn_if_current(&runtime, turn_id, generation, "recovery_unavailable");
                    return Ok(());
                }
            }
            let _ = tts.close(Some(1000), Some("Hermes unavailable"));
            fail_turn_if_current(&runtime, turn_id, generation, code);
            return Ok(());
        }
    };
    // This means the authenticated Hermes origin accepted the streaming
    // request, not merely that the gateway began attempting a connection.
    if !send_device_control(
        &runtime,
        &ControlMessage::ResponseStart {
            v: WIRE_PROTOCOL_VERSION,
            turn_id: TurnId::new(turn_id),
        },
    ) {
        let _ = tts.close(Some(1000), Some("device unavailable"));
        fail_turn_if_current(&runtime, turn_id, generation, "device_send_failed");
        return Ok(());
    }
    {
        let mut state = runtime.borrow_mut();
        let Some(turn) = state
            .turn
            .as_mut()
            .filter(|turn| turn.id == turn_id && turn.generation == generation)
        else {
            let _ = tts.close(Some(1000), Some("stale turn"));
            return Ok(());
        };
        turn.tts = Some(tts.clone());
        set_turn_phase(
            turn,
            TurnPhase::StreamingOutput,
            STREAMING_OUTPUT_TIMEOUT_SECONDS,
        );
    }

    let tts_listener = listen_tts(runtime.clone(), turn_id, generation, tts);
    let hermes_consumer =
        consume_hermes_stream(runtime.clone(), storage, turn_id, generation, hermes);
    let outcome = future::try_join(tts_listener, hermes_consumer).await;
    if current_generation(&runtime, turn_id) != Some(generation) {
        return Ok(());
    }
    if outcome.is_err() {
        fail_turn_if_current(&runtime, turn_id, generation, "stream_pipeline_failed");
    }
    Ok(())
}

async fn open_hermes_stream(
    config: &Config,
    device_id: &str,
    transcript: &str,
    prior_response_id: Option<&str>,
    runtime: &Rc<RefCell<Runtime>>,
    turn_id: u64,
    generation: u64,
) -> std::result::Result<Response, &'static str> {
    let idempotency_key = runtime
        .borrow()
        .turn
        .as_ref()
        .filter(|turn| turn.id == turn_id && turn.generation == generation)
        .and_then(|turn| turn.conversation_id.as_deref())
        .map(|conversation_id| hermes_idempotency_key(conversation_id, turn_id))
        .ok_or("stale_turn")?;
    let endpoint = api_endpoint_url(&config.hermes_base_url, "responses")
        .map_err(|_| "hermes_configuration_failed")?;
    let mut body = json!({
        "model": config.hermes_model,
        "input": transcript,
        "instructions": config.hermes_voice_instructions,
        "stream": true,
        "store": true
    });
    if let Some(prior_response_id) = prior_response_id {
        body["previous_response_id"] = Value::String(prior_response_id.to_string());
    }

    let headers = Headers::new();
    headers
        .set("Content-Type", "application/json")
        .map_err(|_| "hermes_configuration_failed")?;
    headers
        .set("Accept", "text/event-stream")
        .map_err(|_| "hermes_configuration_failed")?;
    headers
        .set(
            "Authorization",
            &format!("Bearer {}", config.hermes_api_key),
        )
        .map_err(|_| "hermes_configuration_failed")?;
    headers
        .set(
            "X-Hermes-Session-Key",
            &config.hermes_session_key(device_id),
        )
        .map_err(|_| "hermes_configuration_failed")?;
    headers
        .set("Idempotency-Key", &idempotency_key)
        .map_err(|_| "hermes_configuration_failed")?;
    if let (Some(client_id), Some(client_secret)) = (
        config.cf_access_client_id.as_deref(),
        config.cf_access_client_secret.as_deref(),
    ) {
        headers
            .set("CF-Access-Client-Id", client_id)
            .map_err(|_| "hermes_configuration_failed")?;
        headers
            .set("CF-Access-Client-Secret", client_secret)
            .map_err(|_| "hermes_configuration_failed")?;
    }

    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        // Manual is workerd's fail-closed redirect mode: any 30x is returned
        // to this code and rejected by the explicit status handling below.
        .with_redirect(RequestRedirect::Manual)
        .with_body(Some(JsValue::from_str(
            &serde_json::to_string(&body).map_err(|_| "hermes_configuration_failed")?,
        )));
    let request = Request::new_with_init(endpoint.as_str(), &init)
        .map_err(|_| "hermes_configuration_failed")?;
    let controller = AbortController::default();
    let signal = controller.signal();
    {
        let mut state = runtime.borrow_mut();
        let Some(turn) = state
            .turn
            .as_mut()
            .filter(|turn| turn.id == turn_id && turn.generation == generation)
        else {
            return Err("stale_turn");
        };
        turn.hermes_abort = Some(controller);
    }
    let mut response = Fetch::Request(request)
        .send_with_signal(&signal)
        .await
        .map_err(|_| "hermes_connect_failed")?;
    match response.status_code() {
        200 => {
            let content_type = response
                .headers()
                .get("Content-Type")
                .map_err(|_| "hermes_connect_failed")?
                .ok_or("hermes_connect_failed")?;
            if !is_event_stream_content_type(&content_type) {
                return Err("hermes_connect_failed");
            }
            let hermes_session_id = response
                .headers()
                .get("X-Hermes-Session-Id")
                .map_err(|_| "hermes_connect_failed")?;
            if hermes_session_id
                .as_deref()
                .is_some_and(|value| !valid_hermes_session_id(value))
            {
                return Err("hermes_connect_failed");
            }
            let mut state = runtime.borrow_mut();
            let Some(turn) = state
                .turn
                .as_mut()
                .filter(|turn| turn.id == turn_id && turn.generation == generation)
            else {
                return Err("stale_turn");
            };
            turn.hermes_session_id = hermes_session_id;
            drop(state);
            mark_stage(runtime, turn_id, generation, "hermes_headers");
            Ok(response)
        }
        404 if prior_response_id.is_some() => {
            let prior_response_id = prior_response_id.expect("matched Some above");
            let body = read_response_limited(&mut response, MAX_HERMES_ERROR_BYTES)
                .await
                .map_err(|_| "hermes_connect_failed")?;
            if is_missing_previous_response_error(&body, prior_response_id) {
                Err("conversation_expired")
            } else {
                Err("hermes_connect_failed")
            }
        }
        429 => Err("hermes_busy"),
        _ => Err("hermes_connect_failed"),
    }
}

fn is_event_stream_content_type(value: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("text/event-stream"))
}

fn hermes_idempotency_key(conversation_id: &str, turn_id: u64) -> String {
    format!("hv2-{conversation_id}-{}", format_turn_id(turn_id))
}

fn is_missing_previous_response_error(body: &[u8], expected_response_id: &str) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    let Some(error) = value.get("error") else {
        return false;
    };
    if error.get("code").and_then(Value::as_str) == Some("previous_response_not_found") {
        return error
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains(expected_response_id));
    }
    let expected_message = format!("Previous response not found: {expected_response_id}");
    error.get("type").and_then(Value::as_str) == Some("invalid_request_error")
        && error.get("param").is_none_or(Value::is_null)
        && error.get("code").is_none_or(Value::is_null)
        && error.get("message").and_then(Value::as_str) == Some(expected_message.as_str())
}

fn is_missing_response_error(body: &[u8], expected_response_id: &str) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    let Some(error) = value.get("error") else {
        return false;
    };
    let expected_message = format!("Response not found: {expected_response_id}");
    error.get("type").and_then(Value::as_str) == Some("invalid_request_error")
        && error.get("param").is_none_or(Value::is_null)
        && error.get("code").is_none_or(Value::is_null)
        && error.get("message").and_then(Value::as_str) == Some(expected_message.as_str())
}

async fn reconcile_inflight_response(
    config: &Config,
    storage: &Storage,
    runtime: &Rc<RefCell<Runtime>>,
    turn_id: u64,
    generation: u64,
    conversation: &ConversationState,
) -> Result<ReconcileOutcome> {
    let Some(journal) = storage.get::<TurnJournal>(STORAGE_TURN_JOURNAL).await? else {
        return Ok(if current_turn_matches(runtime, turn_id, generation) {
            ReconcileOutcome::Current
        } else {
            ReconcileOutcome::Stale
        });
    };
    if !current_turn_matches(runtime, turn_id, generation) {
        return Ok(ReconcileOutcome::Stale);
    }
    if !journal_belongs_to_conversation(&journal, conversation) {
        let recovered = TurnJournal {
            conversation_id: Some(conversation.conversation_id.clone()),
            prior_response_id: conversation.last_completed_response_id.clone(),
            inflight_response_id: None,
            idempotency_key: None,
            state: "recovered_after_conversation_boundary".into(),
            ..journal.clone()
        };
        write_reconciled_journal(storage, &journal, recovered, None, conversation).await?;
        return Ok(if current_turn_matches(runtime, turn_id, generation) {
            ReconcileOutcome::Current
        } else {
            ReconcileOutcome::Stale
        });
    }
    if journal.state == "completed" || journal.state.starts_with("recovered_") {
        return Ok(ReconcileOutcome::Current);
    }
    if journal.state.starts_with("ambiguous_") || journal.state == "starting_hermes" {
        // Hermes main currently ignores Idempotency-Key in the streaming
        // Responses branch. Retrying could execute tools twice, while
        // continuing from the old head could silently fork context. Preserve
        // the journal and require an explicit conversation reset.
        return Ok(ReconcileOutcome::Ambiguous);
    }
    let Some(inflight_id) = journal.inflight_response_id.clone() else {
        if journal.state != "capturing" {
            return Ok(ReconcileOutcome::Ambiguous);
        }
        // Capture ended before Hermes was called, so no tool or response side
        // effect exists and accepting a new turn is safe.
        let mut recovered = journal.clone();
        recovered.state = "recovered_before_hermes".into();
        write_reconciled_journal(storage, &journal, recovered, None, conversation).await?;
        return Ok(if current_turn_matches(runtime, turn_id, generation) {
            ReconcileOutcome::Current
        } else {
            ReconcileOutcome::Stale
        });
    };
    if !valid_response_id(&inflight_id) {
        return Err(Error::RustError(
            "invalid response ID in turn journal".into(),
        ));
    }

    let endpoint = api_endpoint_url(&config.hermes_base_url, &format!("responses/{inflight_id}"))?;
    let headers = Headers::new();
    headers.set("Accept", "application/json")?;
    headers.set(
        "Authorization",
        &format!("Bearer {}", config.hermes_api_key),
    )?;
    if let (Some(client_id), Some(client_secret)) = (
        config.cf_access_client_id.as_deref(),
        config.cf_access_client_secret.as_deref(),
    ) {
        headers.set("CF-Access-Client-Id", client_id)?;
        headers.set("CF-Access-Client-Secret", client_secret)?;
    }
    let mut init = RequestInit::new();
    init.with_method(Method::Get)
        .with_headers(headers)
        // Do not follow a credentialed recovery request. The status handling
        // below accepts only the exact expected success/404 contracts.
        .with_redirect(RequestRedirect::Manual);
    let request = Request::new_with_init(endpoint.as_str(), &init)?;
    let controller = AbortController::default();
    let signal = controller.signal();
    let fetch_request = Fetch::Request(request);
    let fetch = fetch_request.send_with_signal(&signal);
    let timeout = Delay::from(remaining_turn_time(runtime, turn_id, generation)?);
    pin_mut!(fetch, timeout);
    let mut response = match future::select(fetch, timeout).await {
        future::Either::Left((response, _)) => response?,
        future::Either::Right(_) => {
            controller.abort_with_reason("response reconciliation timed out");
            return Err(Error::RustError(
                "Hermes response reconciliation timed out".into(),
            ));
        }
    };
    if !current_turn_matches(runtime, turn_id, generation) {
        controller.abort_with_reason("stale response reconciliation");
        return Ok(ReconcileOutcome::Stale);
    }
    if response.status_code() == 404 {
        let body = read_response_limited(&mut response, MAX_HERMES_ERROR_BYTES).await?;
        if !is_missing_response_error(&body, &inflight_id) {
            return Err(Error::RustError(
                "Hermes response reconciliation route returned 404".into(),
            ));
        }
        let mut ambiguous = journal.clone();
        ambiguous.state = "ambiguous_missing_response".into();
        write_reconciled_journal(storage, &journal, ambiguous, None, conversation).await?;
        return Ok(ReconcileOutcome::Ambiguous);
    }
    if !(200..300).contains(&response.status_code()) {
        return Err(Error::RustError(
            "Hermes response reconciliation failed".into(),
        ));
    }
    let read = read_response_limited(&mut response, config.max_hermes_response_bytes);
    let timeout = Delay::from(remaining_turn_time(runtime, turn_id, generation)?);
    pin_mut!(read, timeout);
    let body = match future::select(read, timeout).await {
        future::Either::Left((body, _)) => body?,
        future::Either::Right(_) => {
            controller.abort_with_reason("response reconciliation timed out");
            return Err(Error::RustError(
                "Hermes response reconciliation timed out".into(),
            ));
        }
    };
    if !current_turn_matches(runtime, turn_id, generation) {
        controller.abort_with_reason("stale response reconciliation");
        return Ok(ReconcileOutcome::Stale);
    }
    let value: Value = serde_json::from_slice(&body)?;
    if value.get("object").and_then(Value::as_str) != Some("response")
        || value.get("id").and_then(Value::as_str) != Some(inflight_id.as_str())
    {
        return Err(Error::RustError(
            "Hermes reconciliation returned an invalid response".into(),
        ));
    }
    if value.get("status").and_then(Value::as_str) == Some("completed") {
        let mut recovered = journal.clone();
        recovered.state = "recovered_completed".into();
        write_reconciled_journal(
            storage,
            &journal,
            recovered,
            Some(inflight_id),
            conversation,
        )
        .await?;
    } else {
        let mut ambiguous = journal.clone();
        ambiguous.state = "ambiguous_incomplete_response".into();
        write_reconciled_journal(storage, &journal, ambiguous, None, conversation).await?;
        return Ok(ReconcileOutcome::Ambiguous);
    }
    Ok(ReconcileOutcome::Current)
}

async fn write_reconciled_journal(
    storage: &Storage,
    expected: &TurnJournal,
    recovered: TurnJournal,
    completed_response_id: Option<String>,
    expected_conversation: &ConversationState,
) -> Result<()> {
    let current: Option<TurnJournal> = storage.get(STORAGE_TURN_JOURNAL).await?;
    if current.as_ref() != Some(expected) {
        return Err(Error::RustError(
            "turn journal changed during reconciliation".into(),
        ));
    }
    if let Some(completed_response_id) = completed_response_id {
        let mut conversation = load_conversation_state(storage).await?;
        if conversation.conversation_id != expected_conversation.conversation_id {
            return Err(Error::RustError(
                "conversation changed during reconciliation".into(),
            ));
        }
        conversation.last_completed_response_id = Some(completed_response_id);
        conversation.last_activity_unix_ms = Some(unix_now_ms());
        validate_conversation_state(&conversation)?;
        storage
            .put_multiple(CompletionWrite {
                conversation_state: conversation,
                turn_journal: recovered,
            })
            .await
    } else {
        storage.put(STORAGE_TURN_JOURNAL, recovered).await
    }
}

async fn read_response_limited(response: &mut Response, limit: usize) -> Result<Vec<u8>> {
    if let Some(length) = response.headers().get("Content-Length")? {
        let length = length
            .parse::<usize>()
            .map_err(|_| Error::RustError("invalid upstream content length".into()))?;
        if length > limit {
            return Err(Error::RustError("upstream response is too large".into()));
        }
    }
    let mut bytes = Vec::new();
    let mut stream = response.stream()?;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(Error::RustError("upstream response is too large".into()));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn consume_hermes_stream(
    runtime: Rc<RefCell<Runtime>>,
    storage: Rc<Storage>,
    turn_id: u64,
    generation: u64,
    mut response: Response,
) -> Result<()> {
    let content_type = response.headers().get("Content-Type")?.unwrap_or_default();
    if !is_event_stream_content_type(&content_type) {
        return Err(Error::RustError(
            "Hermes did not return an SSE response".into(),
        ));
    }
    let mut parser = SseParser::new_with_discard(
        SseLimits {
            // Hermes deliberately targets a terminal response envelope below
            // roughly 100 KiB, so 128 KiB is the retained-event ceiling.
            max_line_bytes: MAX_HERMES_SSE_EVENT_BYTES,
            max_event_bytes: MAX_HERMES_SSE_EVENT_BYTES,
        },
        SseDiscardPolicy {
            should_discard: should_discard_hermes_event,
            max_event_bytes: MAX_HERMES_DISCARDED_EVENT_BYTES,
        },
    )
    .map_err(|error| Error::RustError(error.to_string()))?;
    let mut segmenter = PhraseSegmenter::new(PhraseConfig::default())
        .map_err(|error| Error::RustError(error.to_string()))?;
    let mut sanitizer = StreamingTtsSanitizer::default();
    let mut response_id: Option<String> = None;
    let mut spoken_chars = 0_usize;
    let mut first_delta = true;
    let mut first_phrase = true;
    let mut total_bytes = 0_usize;
    let mut stream = response.stream()?;

    loop {
        if current_generation(&runtime, turn_id) != Some(generation) {
            return Ok(());
        }
        let next = stream.next();
        let timeout = Delay::from(remaining_turn_time(&runtime, turn_id, generation)?);
        pin_mut!(next, timeout);
        let chunk = match future::select(next, timeout).await {
            future::Either::Left((Some(chunk), _)) => chunk?,
            future::Either::Left((None, _)) => break,
            future::Either::Right(_) => {
                return Err(Error::RustError("Hermes stream timed out".into()));
            }
        };
        total_bytes = total_bytes
            .checked_add(chunk.len())
            .ok_or_else(|| Error::RustError("Hermes stream exceeded its bound".into()))?;
        if total_bytes > MAX_HERMES_STREAM_BYTES {
            return Err(Error::RustError("Hermes stream exceeded its bound".into()));
        }
        let events = parser
            .push(&chunk)
            .map_err(|error| Error::RustError(error.to_string()))?;
        for event in events {
            if apply_hermes_event(
                &runtime,
                &storage,
                turn_id,
                generation,
                event.event.as_str(),
                &event.data,
                &mut response_id,
                &mut segmenter,
                &mut sanitizer,
                &mut spoken_chars,
                &mut first_delta,
                &mut first_phrase,
            )
            .await?
            {
                return Ok(());
            }
        }
    }

    for event in parser
        .finish()
        .map_err(|error| Error::RustError(error.to_string()))?
    {
        if apply_hermes_event(
            &runtime,
            &storage,
            turn_id,
            generation,
            event.event.as_str(),
            &event.data,
            &mut response_id,
            &mut segmenter,
            &mut sanitizer,
            &mut spoken_chars,
            &mut first_delta,
            &mut first_phrase,
        )
        .await?
        {
            return Ok(());
        }
    }
    Err(Error::RustError(
        "Hermes stream ended without response.completed".into(),
    ))
}

fn should_discard_hermes_event(event_name: &str) -> bool {
    !matches!(
        event_name,
        "response.created"
            | "response.output_text.delta"
            | "response.completed"
            | "response.failed"
    )
}

#[allow(clippy::too_many_arguments)]
async fn apply_hermes_event(
    runtime: &Rc<RefCell<Runtime>>,
    storage: &Rc<Storage>,
    turn_id: u64,
    generation: u64,
    event_name: &str,
    data: &Value,
    response_id: &mut Option<String>,
    segmenter: &mut PhraseSegmenter,
    sanitizer: &mut StreamingTtsSanitizer,
    spoken_chars: &mut usize,
    first_delta: &mut bool,
    first_phrase: &mut bool,
) -> Result<bool> {
    match event_name {
        "response.created" => {
            if response_id.is_some() {
                return Err(Error::RustError(
                    "Hermes emitted duplicate response.created".into(),
                ));
            }
            let id = data
                .get("response")
                .and_then(|response| response.get("id"))
                .and_then(Value::as_str)
                .filter(|id| valid_response_id(id))
                .ok_or_else(|| Error::RustError("Hermes response.created is invalid".into()))?
                .to_string();
            *response_id = Some(id.clone());
            let (conversation_id, prior_response_id) = {
                let mut state = runtime.borrow_mut();
                let turn = state
                    .turn
                    .as_mut()
                    .filter(|turn| turn.id == turn_id && turn.generation == generation)
                    .ok_or_else(|| Error::RustError("stale Hermes response".into()))?;
                turn.response_id = Some(id.clone());
                (
                    turn.conversation_id
                        .clone()
                        .ok_or_else(|| Error::RustError("turn has no conversation".into()))?,
                    turn.prior_response_id.clone(),
                )
            };
            write_current_turn_journal(
                storage,
                runtime,
                turn_id,
                generation,
                TurnJournal {
                    turn_id: format_turn_id(turn_id),
                    conversation_id: Some(conversation_id.clone()),
                    prior_response_id,
                    inflight_response_id: Some(id),
                    idempotency_key: Some(hermes_idempotency_key(&conversation_id, turn_id)),
                    state: "hermes_in_progress".into(),
                },
            )
            .await?;
            mark_stage(runtime, turn_id, generation, "hermes_created");
        }
        "response.output_text.delta" => {
            let delta = data
                .get("delta")
                .and_then(Value::as_str)
                .filter(|delta| delta.len() <= MAX_HERMES_SSE_EVENT_BYTES)
                .ok_or_else(|| Error::RustError("Hermes text delta is invalid".into()))?;
            if *first_delta {
                *first_delta = false;
                mark_stage(runtime, turn_id, generation, "hermes_first_delta");
            }
            for phrase in segmenter.push(delta) {
                if send_tts_phrase(
                    runtime,
                    turn_id,
                    generation,
                    &phrase,
                    sanitizer,
                    spoken_chars,
                )? && *first_phrase
                {
                    *first_phrase = false;
                    mark_stage(runtime, turn_id, generation, "tts_first_phrase");
                }
            }
        }
        "response.completed" => {
            let completed = data
                .get("response")
                .ok_or_else(|| Error::RustError("Hermes completion is missing".into()))?;
            if completed.get("status").and_then(Value::as_str) != Some("completed") {
                return Err(Error::RustError("Hermes response was not completed".into()));
            }
            let completed_id = completed
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| valid_response_id(id))
                .ok_or_else(|| Error::RustError("Hermes completion ID is invalid".into()))?;
            if response_id.as_deref() != Some(completed_id) {
                return Err(Error::RustError(
                    "Hermes completion ID did not match response.created".into(),
                ));
            }

            // Hermes completion is the authoritative conversation boundary.
            // Promote it before any fallible speech work so a TTS failure does
            // not roll back tool/reasoning state that Hermes already committed.
            promote_current_response(storage, runtime, turn_id, generation, completed_id).await?;
            if let Some(turn) = runtime.borrow_mut().turn.as_mut() {
                if turn.id == turn_id && turn.generation == generation {
                    turn.hermes_abort = None;
                }
            }
            mark_stage(runtime, turn_id, generation, "hermes_completed");

            if let Some(phrase) = segmenter.finish() {
                if send_tts_phrase(
                    runtime,
                    turn_id,
                    generation,
                    &phrase,
                    sanitizer,
                    spoken_chars,
                )? && *first_phrase
                {
                    *first_phrase = false;
                    mark_stage(runtime, turn_id, generation, "tts_first_phrase");
                }
            }
            if *spoken_chars == 0 {
                return Err(Error::RustError("Hermes returned no speakable text".into()));
            }
            let tts = runtime
                .borrow()
                .turn
                .as_ref()
                .filter(|turn| turn.id == turn_id && turn.generation == generation)
                .and_then(|turn| turn.tts.clone())
                .ok_or_else(|| Error::RustError("TTS websocket is unavailable".into()))?;
            tts.send(&json!({
                "context_id": format_turn_id(turn_id),
                "flush": true
            }))?;
            sanitizer.reset();
            return Ok(true);
        }
        "response.failed" => {
            return Err(Error::RustError("Hermes reported response.failed".into()));
        }
        // Tool arguments and results can contain host secrets. They are
        // intentionally ignored and never logged or proxied to the device.
        "response.output_item.added"
        | "response.output_item.done"
        | "response.output_text.done" => {}
        _ => {}
    }
    Ok(false)
}

fn send_tts_phrase(
    runtime: &Rc<RefCell<Runtime>>,
    turn_id: u64,
    generation: u64,
    raw_phrase: &str,
    sanitizer: &mut StreamingTtsSanitizer,
    spoken_chars: &mut usize,
) -> Result<bool> {
    let cleaned = sanitizer.push_phrase(raw_phrase);
    if cleaned.is_empty() {
        return Ok(false);
    }
    let phrase_chars = cleaned.chars().count();
    if spoken_chars.saturating_add(phrase_chars) > MAX_TTS_CHARS {
        return Err(Error::RustError(
            "Hermes response exceeds the configured speech limit".into(),
        ));
    }
    *spoken_chars += phrase_chars;
    let tts = runtime
        .borrow()
        .turn
        .as_ref()
        .filter(|turn| turn.id == turn_id && turn.generation == generation)
        .and_then(|turn| turn.tts.clone())
        .ok_or_else(|| Error::RustError("TTS websocket is unavailable".into()))?;
    tts.send(&json!({
        "context_id": format_turn_id(turn_id),
        "text": format!("{cleaned} ")
    }))?;
    Ok(true)
}

fn valid_response_id(id: &str) -> bool {
    id.starts_with("resp_")
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

async fn listen_tts(
    runtime: Rc<RefCell<Runtime>>,
    turn_id: u64,
    generation: u64,
    tts: WebSocket,
) -> Result<()> {
    let mut events =
        BoundedWebSocketEvents::new(&tts, MAX_TTS_EVENT_BYTES, PROVIDER_EVENT_QUEUE_CAPACITY)?;
    tts.accept()?;
    let context_id = format_turn_id(turn_id);
    let max_output_samples = runtime
        .borrow()
        .turn
        .as_ref()
        .filter(|turn| turn.id == turn_id && turn.generation == generation)
        .map(|turn| turn.max_output_samples)
        .ok_or_else(|| Error::RustError("stale TTS turn".into()))?;
    let (ack_tx, mut ack_rx) = channel(1);
    {
        let mut state = runtime.borrow_mut();
        let turn = state
            .turn
            .as_mut()
            .filter(|turn| turn.id == turn_id && turn.generation == generation)
            .ok_or_else(|| Error::RustError("stale TTS turn".into()))?;
        turn.output_ack_tx = Some(ack_tx);
    }
    tts.send(&json!({
        "text": " ",
        "context_id": context_id,
        "voice_settings": {
            "stability": 0.5,
            "similarity_boost": 0.8,
            "speed": 1.0
        }
    }))?;
    let mut audio_started = false;
    let mut provider_audio_bytes = 0_u64;
    let mut reframer = PlaybackReframer::default();

    loop {
        if current_generation(&runtime, turn_id) != Some(generation) {
            return Ok(());
        }
        let remaining = remaining_turn_time(&runtime, turn_id, generation)?;
        let event = events.next();
        let keepalive = Delay::from(remaining.min(Duration::from_secs(TTS_KEEPALIVE_SECONDS)));
        pin_mut!(event, keepalive);
        let event = match future::select(event, keepalive).await {
            future::Either::Left((event, _)) => {
                event?.ok_or_else(|| Error::RustError("TTS websocket ended early".into()))?
            }
            future::Either::Right(_) => {
                if remaining_turn_time(&runtime, turn_id, generation).is_err() {
                    return Err(Error::RustError("TTS stream timed out".into()));
                }
                // ElevenLabs closes an idle multi-context stream. An empty
                // text update resets its inactivity timer without producing
                // speech while Hermes is still running tools.
                tts.send(&json!({
                    "context_id": context_id,
                    "text": ""
                }))?;
                continue;
            }
        };
        match event {
            ProviderEvent::Text(text) => {
                let value: Value = serde_json::from_str(&text)?;
                let carries_context_payload = value.get("audio").is_some()
                    || value.get("is_final").is_some()
                    || value.get("isFinal").is_some();
                if carries_context_payload
                    && value.get("contextId").and_then(Value::as_str) != Some(context_id.as_str())
                {
                    return Err(Error::RustError(
                        "TTS event has a missing or unexpected context ID".into(),
                    ));
                }
                if let Some(encoded) = value.get("audio").and_then(Value::as_str) {
                    if encoded.len() > MAX_PROVIDER_AUDIO_BYTES * 2 {
                        return Err(Error::RustError("TTS audio event is too large".into()));
                    }
                    let audio = BASE64
                        .decode(encoded)
                        .map_err(|_| Error::RustError("TTS returned invalid audio".into()))?;
                    if audio.is_empty() || audio.len() > MAX_PROVIDER_AUDIO_BYTES {
                        return Err(Error::RustError("TTS returned invalid PCM".into()));
                    }
                    provider_audio_bytes = checked_output_audio_total(
                        provider_audio_bytes,
                        audio.len(),
                        max_output_samples,
                    )
                    .map_err(|message| Error::RustError(message.into()))?;
                    if !audio_started {
                        audio_started = true;
                        mark_stage(&runtime, turn_id, generation, "tts_first_audio");
                        if !send_device_control(
                            &runtime,
                            &ControlMessage::TtsStart {
                                v: WIRE_PROTOCOL_VERSION,
                                turn_id: TurnId::new(turn_id),
                                sample_rate: AUDIO_SAMPLE_RATE,
                            },
                        ) {
                            return Err(Error::RustError("device rejected tts.start".into()));
                        }
                    }
                    for frame in reframer.push(&audio) {
                        send_playback_frame(&runtime, turn_id, generation, &frame, &mut ack_rx)
                            .await?;
                    }
                }
                let is_final = value
                    .get("is_final")
                    .or_else(|| value.get("isFinal"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if is_final {
                    if !audio_started {
                        return Err(Error::RustError("TTS returned no audio".into()));
                    }
                    if let Some(frame) = reframer
                        .finish()
                        .map_err(|message| Error::RustError(message.into()))?
                    {
                        send_playback_frame(&runtime, turn_id, generation, &frame, &mut ack_rx)
                            .await?;
                    }
                    let last_seq = runtime
                        .borrow()
                        .turn
                        .as_ref()
                        .filter(|turn| turn.id == turn_id && turn.generation == generation)
                        .and_then(|turn| turn.next_output_seq.checked_sub(1))
                        .ok_or_else(|| Error::RustError("TTS sequence is empty".into()))?;
                    if !send_device_control(
                        &runtime,
                        &ControlMessage::TtsEnd {
                            v: WIRE_PROTOCOL_VERSION,
                            turn_id: TurnId::new(turn_id),
                            last_seq,
                        },
                    ) {
                        return Err(Error::RustError("device rejected tts.end".into()));
                    }
                    mark_stage(&runtime, turn_id, generation, "tts_final");
                    wait_for_output_ack(&runtime, turn_id, generation, last_seq, &mut ack_rx)
                        .await?;
                    mark_stage(&runtime, turn_id, generation, "output_final_ack");
                    if !send_device_control(
                        &runtime,
                        &ControlMessage::TurnDone {
                            v: WIRE_PROTOCOL_VERSION,
                            turn_id: TurnId::new(turn_id),
                            cancelled: Some(false),
                            reason: None,
                        },
                    ) {
                        return Err(Error::RustError("device rejected turn.done".into()));
                    }
                    let _ = tts.send(&json!({"close_socket": true}));
                    runtime.borrow_mut().turn = None;
                    return Ok(());
                }
            }
            ProviderEvent::Close => {
                return Err(Error::RustError("TTS websocket closed early".into()));
            }
            ProviderEvent::Error => {
                return Err(Error::RustError("TTS websocket reported an error".into()));
            }
        }
    }
}

fn checked_output_audio_total(
    current_bytes: u64,
    additional_bytes: usize,
    max_output_samples: u32,
) -> std::result::Result<u64, &'static str> {
    let next_bytes = current_bytes
        .checked_add(u64::try_from(additional_bytes).map_err(|_| "TTS audio length overflow")?)
        .ok_or("TTS audio length overflow")?;
    let maximum_bytes = u64::from(max_output_samples).saturating_mul(2);
    if next_bytes > maximum_bytes {
        return Err("TTS audio exceeds the configured turn limit");
    }
    Ok(next_bytes)
}

async fn send_playback_frame(
    runtime: &Rc<RefCell<Runtime>>,
    turn_id: u64,
    generation: u64,
    frame_audio: &[u8],
    ack_rx: &mut Receiver<u32>,
) -> Result<()> {
    remaining_turn_time(runtime, turn_id, generation)?;
    if frame_audio.is_empty()
        || frame_audio.len() > DEVICE_AUDIO_CHUNK_BYTES
        || frame_audio.len() & 1 != 0
    {
        return Err(Error::RustError("invalid reframed TTS PCM".into()));
    }
    wait_for_output_capacity(runtime, turn_id, generation, ack_rx).await?;
    let (device, sequence, first_sample) = reserve_output_frame(
        runtime,
        turn_id,
        generation,
        u32::try_from(frame_audio.len() / 2).unwrap_or(u32::MAX),
    )?;
    let frame = encode_audio_frame(
        &AudioHeader {
            kind: AudioKind::PlaybackPcm,
            flags: AudioFlags::NONE,
            turn_id: TurnId::new(turn_id),
            sequence,
            first_sample,
        },
        frame_audio,
        MAX_AUDIO_PAYLOAD_BYTES,
    )
    .map_err(|error| Error::RustError(error.to_string()))?;
    device.send_with_bytes(frame)?;
    if sequence == 0 {
        mark_stage(runtime, turn_id, generation, "first_output_frame_sent");
    }
    Ok(())
}

fn reserve_output_frame(
    runtime: &Rc<RefCell<Runtime>>,
    turn_id: u64,
    generation: u64,
    samples: u32,
) -> Result<(WebSocket, u32, u32)> {
    let mut state = runtime.borrow_mut();
    let device = state
        .device
        .clone()
        .ok_or_else(|| Error::RustError("device disconnected".into()))?;
    let turn = state
        .turn
        .as_mut()
        .filter(|turn| turn.id == turn_id && turn.generation == generation)
        .ok_or_else(|| Error::RustError("stale TTS audio".into()))?;
    let sequence = turn.next_output_seq;
    let first_sample = turn.output_samples;
    let next_sequence = turn
        .next_output_seq
        .checked_add(1)
        .ok_or_else(|| Error::RustError("TTS sequence overflow".into()))?;
    let next_output_samples = turn
        .output_samples
        .checked_add(samples)
        .filter(|total| *total <= turn.max_output_samples)
        .ok_or_else(|| Error::RustError("TTS audio exceeds the configured turn limit".into()))?;
    turn.next_output_seq = next_sequence;
    turn.output_samples = next_output_samples;
    Ok((device, sequence, first_sample))
}

async fn wait_for_output_capacity(
    runtime: &Rc<RefCell<Runtime>>,
    turn_id: u64,
    generation: u64,
    ack_rx: &mut Receiver<u32>,
) -> Result<()> {
    loop {
        let has_capacity = {
            let state = runtime.borrow();
            let device = state
                .device
                .as_ref()
                .ok_or_else(|| Error::RustError("device disconnected".into()))?;
            let turn = state
                .turn
                .as_ref()
                .filter(|turn| turn.id == turn_id && turn.generation == generation)
                .ok_or_else(|| Error::RustError("stale output turn".into()))?;
            let acknowledged = turn.last_output_ack.map_or(0, |seq| seq.saturating_add(1));
            turn.next_output_seq.saturating_sub(acknowledged) < OUTPUT_WINDOW_FRAMES
                && device.as_ref().buffered_amount() <= MAX_DEVICE_BUFFERED_BYTES
        };
        if has_capacity {
            return Ok(());
        }
        wait_for_ack_event(ack_rx, runtime, turn_id, generation).await?;
    }
}

async fn wait_for_output_ack(
    runtime: &Rc<RefCell<Runtime>>,
    turn_id: u64,
    generation: u64,
    expected: u32,
    ack_rx: &mut Receiver<u32>,
) -> Result<()> {
    loop {
        let acknowledged = runtime
            .borrow()
            .turn
            .as_ref()
            .filter(|turn| turn.id == turn_id && turn.generation == generation)
            .and_then(|turn| turn.last_output_ack)
            .is_some_and(|sequence| sequence >= expected);
        if acknowledged {
            return Ok(());
        }
        wait_for_ack_event(ack_rx, runtime, turn_id, generation).await?;
    }
}

async fn wait_for_ack_event(
    ack_rx: &mut Receiver<u32>,
    runtime: &Rc<RefCell<Runtime>>,
    turn_id: u64,
    generation: u64,
) -> Result<()> {
    let ack = ack_rx.next();
    let timeout = Delay::from(
        Duration::from_secs(OUTPUT_ACK_TIMEOUT_SECONDS)
            .min(remaining_turn_time(runtime, turn_id, generation)?),
    );
    pin_mut!(ack, timeout);
    match future::select(ack, timeout).await {
        future::Either::Left((Some(_), _)) => Ok(()),
        future::Either::Left((None, _)) => Err(Error::RustError(
            "output acknowledgement channel closed".into(),
        )),
        future::Either::Right(_) => {
            Err(Error::RustError("output acknowledgement timed out".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermes_parser_retains_only_voice_control_events() {
        for event in [
            "response.created",
            "response.output_text.delta",
            "response.completed",
            "response.failed",
        ] {
            assert!(!should_discard_hermes_event(event), "{event}");
        }
        for event in [
            "response.output_item.added",
            "response.output_item.done",
            "response.output_text.done",
            "response.function_call_arguments.delta",
            "future.extension",
        ] {
            assert!(should_discard_hermes_event(event), "{event}");
        }
    }

    #[test]
    fn atomic_start_write_uses_durable_storage_key_names() {
        let conversation_state = ConversationState {
            version: CONVERSATION_STATE_VERSION,
            conversation_id: "12345678-1234-1234-1234-123456789abc".into(),
            binding: None,
            last_completed_response_id: None,
            hermes_session_id: None,
            last_activity_unix_ms: None,
            last_reset_request_id: None,
        };
        let value = serde_json::to_value(StartWrite {
            last_seen_turn_id: "0000000000000042".into(),
            conversation_state,
            turn_journal: TurnJournal {
                turn_id: "0000000000000042".into(),
                conversation_id: Some("12345678-1234-1234-1234-123456789abc".into()),
                prior_response_id: None,
                inflight_response_id: None,
                idempotency_key: None,
                state: "capturing".into(),
            },
        })
        .unwrap();
        assert!(value.get(STORAGE_LAST_SEEN_TURN).is_some());
        assert!(value.get(STORAGE_CONVERSATION_STATE).is_some());
        assert!(value.get(STORAGE_TURN_JOURNAL).is_some());
        assert_eq!(value.as_object().unwrap().len(), 3);
    }

    #[test]
    fn optional_idle_conversation_rotation_has_an_exact_boundary() {
        let conversation = ConversationState {
            version: CONVERSATION_STATE_VERSION,
            conversation_id: "12345678-1234-1234-1234-123456789abc".into(),
            binding: None,
            last_completed_response_id: Some("resp_0123456789abcdef0123456789ab".into()),
            hermes_session_id: Some("session-alpha".into()),
            last_activity_unix_ms: Some(1_000_000.0),
            last_reset_request_id: None,
        };
        assert!(!conversation_idle_expired(&conversation, None, 9_000_000.0));
        assert!(!conversation_idle_expired(
            &conversation,
            Some(60),
            1_059_999.0
        ));
        assert!(conversation_idle_expired(
            &conversation,
            Some(60),
            1_060_000.0
        ));
        assert!(!conversation_idle_expired(
            &conversation,
            Some(60),
            999_999.0
        ));
    }

    #[test]
    fn conversation_state_rejects_cross_scope_or_malformed_identifiers() {
        let valid = ConversationState {
            version: CONVERSATION_STATE_VERSION,
            conversation_id: "12345678-1234-1234-1234-123456789abc".into(),
            binding: Some(ConversationBinding {
                fingerprint: "a".repeat(64),
                hermes_base_url: String::new(),
                hermes_model: String::new(),
                hermes_session_key: String::new(),
                hermes_profile_id: String::new(),
                binding_revision: String::new(),
            }),
            last_completed_response_id: Some("resp_0123456789abcdef0123456789ab".into()),
            hermes_session_id: Some("session-alpha".into()),
            last_activity_unix_ms: Some(1_000_000.0),
            last_reset_request_id: Some("0123456789abcdef".into()),
        };
        assert!(validate_conversation_state(&valid).is_ok());

        let mut invalid = valid.clone();
        invalid.conversation_id = "contains/slash".into();
        assert!(validate_conversation_state(&invalid).is_err());
        let mut invalid = valid.clone();
        invalid.last_completed_response_id = Some("not-a-response".into());
        assert!(validate_conversation_state(&invalid).is_err());
        let mut invalid = valid.clone();
        invalid.binding.as_mut().unwrap().fingerprint = "A".repeat(64);
        assert!(validate_conversation_state(&invalid).is_err());
        let mut invalid = valid.clone();
        invalid.binding.as_mut().unwrap().hermes_base_url = "https://private.example".into();
        assert!(validate_conversation_state(&invalid).is_err());
        let mut invalid = valid;
        invalid.last_reset_request_id = Some("UPPERCASE00000000".into());
        assert!(validate_conversation_state(&invalid).is_err());
    }

    #[test]
    fn conversation_boundary_fences_old_and_legacy_inflight_journals() {
        let mut conversation = ConversationState {
            version: CONVERSATION_STATE_VERSION,
            conversation_id: "12345678-1234-1234-1234-123456789abc".into(),
            binding: None,
            last_completed_response_id: Some("resp_safe".into()),
            hermes_session_id: None,
            last_activity_unix_ms: None,
            last_reset_request_id: None,
        };
        let mut journal = TurnJournal {
            turn_id: "0000000000000042".into(),
            conversation_id: Some(conversation.conversation_id.clone()),
            prior_response_id: Some("resp_safe".into()),
            inflight_response_id: Some("resp_candidate".into()),
            idempotency_key: Some("hv2-test".into()),
            state: "hermes_in_progress".into(),
        };
        assert!(journal_belongs_to_conversation(&journal, &conversation));

        journal.conversation_id = Some("other-conversation".into());
        assert!(!journal_belongs_to_conversation(&journal, &conversation));

        // Pre-lifecycle journals had no conversation_id. They may be migrated
        // only when their safe prior head matches and no explicit reset exists.
        journal.conversation_id = None;
        assert!(journal_belongs_to_conversation(&journal, &conversation));
        journal.prior_response_id = Some("resp_other".into());
        assert!(!journal_belongs_to_conversation(&journal, &conversation));
        journal.prior_response_id = Some("resp_safe".into());
        conversation.last_reset_request_id = Some("0123456789abcdef".into());
        assert!(!journal_belongs_to_conversation(&journal, &conversation));
    }

    #[test]
    fn hermes_404_classification_requires_the_exact_structured_error() {
        let response_id = "resp_0123456789abcdef0123456789ab";
        let previous = json!({
            "error": {
                "message": format!("Previous response not found: {response_id}"),
                "type": "invalid_request_error",
                "param": null,
                "code": null
            }
        });
        assert!(is_missing_previous_response_error(
            &serde_json::to_vec(&previous).unwrap(),
            response_id
        ));
        assert!(!is_missing_previous_response_error(
            br#"{"error":{"message":"Route not found","type":"invalid_request_error","param":null,"code":null}}"#,
            response_id
        ));
        assert!(!is_missing_previous_response_error(
            &serde_json::to_vec(&previous).unwrap(),
            "resp_different"
        ));

        let retrieval = json!({
            "error": {
                "message": format!("Response not found: {response_id}"),
                "type": "invalid_request_error",
                "param": null,
                "code": null
            }
        });
        assert!(is_missing_response_error(
            &serde_json::to_vec(&retrieval).unwrap(),
            response_id
        ));
        assert!(!is_missing_response_error(
            br#"{"error":{"message":"Not found","type":"invalid_request_error","param":null,"code":null}}"#,
            response_id
        ));
    }

    #[test]
    fn hermes_turn_idempotency_is_deterministic_and_conversation_scoped() {
        let first = hermes_idempotency_key("conversation-a", 42);
        assert_eq!(first, hermes_idempotency_key("conversation-a", 42));
        assert_ne!(first, hermes_idempotency_key("conversation-a", 43));
        assert_ne!(first, hermes_idempotency_key("conversation-b", 42));
        assert!(first.starts_with("hv2-conversation-a-"));
        assert!(first.len() < 128);
    }

    #[test]
    fn hermes_stream_content_type_requires_the_exact_media_type() {
        for valid in [
            "text/event-stream",
            "Text/Event-Stream",
            " text/event-stream ; charset=utf-8",
        ] {
            assert!(is_event_stream_content_type(valid), "{valid}");
        }
        for invalid in [
            "",
            "text/html",
            "application/json",
            "text/event-stream-malformed",
            "application/text/event-stream",
        ] {
            assert!(!is_event_stream_content_type(invalid), "{invalid}");
        }
    }

    #[test]
    fn public_error_codes_hide_internal_provider_details() {
        assert_eq!(
            public_error_code("input_sequence_mismatch"),
            "sequence_error"
        );
        assert_eq!(public_error_code("stt_send_failed"), "stt_failed");
        assert_eq!(
            public_error_code("conversation_ambiguous"),
            "conversation_ambiguous"
        );
        assert_eq!(
            public_error_code("unexpected_internal_detail"),
            "internal_error"
        );
    }

    #[test]
    fn input_credit_is_a_cumulative_32_frame_window() {
        assert!(input_credit_available(31, None));
        assert!(!input_credit_available(32, None));
        assert!(input_credit_available(32, Some(0)));
        assert!(!input_credit_available(u32::MAX, Some(0)));
    }

    #[test]
    fn legacy_raw_bindings_are_readable_only_for_rotation() {
        let binding: ConversationBinding = serde_json::from_value(json!({
            "hermes_base_url": "https://hermes.example.com",
            "hermes_model": "hermes-agent",
            "hermes_session_key": "memory:kitchen"
        }))
        .unwrap();
        assert!(binding.fingerprint.is_empty());
        assert!(valid_conversation_binding(&binding));
    }

    #[test]
    fn current_binding_storage_contains_only_a_one_way_fingerprint() {
        let binding = ConversationBinding {
            fingerprint: "b".repeat(64),
            hermes_base_url: String::new(),
            hermes_model: String::new(),
            hermes_session_key: String::new(),
            hermes_profile_id: String::new(),
            binding_revision: String::new(),
        };
        let value = serde_json::to_value(binding).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 1);
        assert_eq!(value["fingerprint"], "b".repeat(64));
    }

    #[test]
    fn legacy_journals_deserialize_without_claiming_idempotency() {
        let journal: TurnJournal = serde_json::from_value(json!({
            "turn_id": "000000000000002a",
            "conversation_id": "conversation-a",
            "prior_response_id": null,
            "inflight_response_id": null,
            "state": "capturing"
        }))
        .unwrap();
        assert_eq!(journal.idempotency_key, None);
    }

    #[test]
    fn playback_reframer_ignores_irregular_provider_event_boundaries() {
        let source: Vec<u8> = (0..8_000).map(|index| (index % 251) as u8).collect();
        let event_lengths = [3, 2_046, 5, 4_097, 1_849];
        assert_eq!(event_lengths.iter().sum::<usize>(), source.len());

        let mut reframer = PlaybackReframer::default();
        let mut frames = Vec::new();
        let mut offset = 0;
        for event_length in event_lengths {
            frames.extend(reframer.push(&source[offset..offset + event_length]));
            assert!(frames
                .iter()
                .all(|frame| frame.len() == DEVICE_AUDIO_CHUNK_BYTES));
            offset += event_length;
        }
        if let Some(final_frame) = reframer.finish().unwrap() {
            frames.push(final_frame);
        }

        assert!(frames[..frames.len() - 1]
            .iter()
            .all(|frame| frame.len() == DEVICE_AUDIO_CHUNK_BYTES));
        assert!(frames.last().unwrap().len() < DEVICE_AUDIO_CHUNK_BYTES);
        assert_eq!(frames.iter().flatten().copied().collect::<Vec<_>>(), source);
    }

    #[test]
    fn playback_reframer_emits_no_empty_terminal_frame_for_exact_multiple() {
        let mut reframer = PlaybackReframer::default();
        let source = vec![0x55; DEVICE_AUDIO_CHUNK_BYTES * 2];
        let frames = reframer.push(&source);
        assert_eq!(
            frames.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![DEVICE_AUDIO_CHUNK_BYTES, DEVICE_AUDIO_CHUNK_BYTES]
        );
        assert_eq!(reframer.finish().unwrap(), None);
    }

    #[test]
    fn playback_reframer_rejects_an_incomplete_terminal_sample() {
        let mut reframer = PlaybackReframer::default();
        assert!(reframer.push(&[1, 2, 3]).is_empty());
        assert_eq!(
            reframer.finish(),
            Err("TTS ended with an incomplete PCM sample")
        );
    }

    #[test]
    fn output_audio_budget_is_exact_across_irregular_provider_chunks() {
        let maximum_samples = AUDIO_SAMPLE_RATE;
        let mut total = 0;
        for chunk_bytes in [2_051, 29_949] {
            total = checked_output_audio_total(total, chunk_bytes, maximum_samples).unwrap();
        }
        assert_eq!(total, u64::from(AUDIO_SAMPLE_RATE) * 2);
        assert_eq!(
            checked_output_audio_total(total, 2, maximum_samples),
            Err("TTS audio exceeds the configured turn limit")
        );
        assert!(checked_output_audio_total(u64::MAX, 2, maximum_samples).is_err());
    }

    #[test]
    fn durable_usage_budget_resets_only_after_a_full_window() {
        let existing = UsageBudget {
            version: USAGE_BUDGET_VERSION,
            window_started_unix_ms: 1_000,
            message_count: 99,
            turn_attempt_count: 7,
            audio_bytes: 12_345,
        };
        assert_eq!(
            usage_budget_for_time(existing.clone(), 1_000 + USAGE_WINDOW_MS - 1).unwrap(),
            existing
        );
        assert_eq!(
            usage_budget_for_time(existing.clone(), 999).unwrap(),
            existing
        );
        assert_eq!(
            usage_budget_for_time(existing, 1_000 + USAGE_WINDOW_MS).unwrap(),
            fresh_usage_budget(1_000 + USAGE_WINDOW_MS)
        );
    }

    #[test]
    fn durable_usage_budget_rejects_unknown_or_zero_epoch_state() {
        for budget in [
            UsageBudget {
                version: USAGE_BUDGET_VERSION + 1,
                window_started_unix_ms: 1,
                message_count: 0,
                turn_attempt_count: 0,
                audio_bytes: 0,
            },
            UsageBudget {
                version: USAGE_BUDGET_VERSION,
                window_started_unix_ms: 0,
                message_count: 0,
                turn_attempt_count: 0,
                audio_bytes: 0,
            },
        ] {
            assert!(usage_budget_for_time(budget, 10).is_err());
        }
    }

    #[test]
    fn durable_usage_budget_is_exhausted_at_each_exact_limit() {
        let base = UsageBudget {
            version: USAGE_BUDGET_VERSION,
            window_started_unix_ms: 1,
            message_count: 9,
            turn_attempt_count: 4,
            audio_bytes: 99,
        };
        assert!(!usage_budget_reached_limits(&base, (10, 5, 100)));
        for exhausted in [
            UsageBudget {
                message_count: 10,
                ..base.clone()
            },
            UsageBudget {
                turn_attempt_count: 5,
                ..base.clone()
            },
            UsageBudget {
                audio_bytes: 100,
                ..base.clone()
            },
        ] {
            assert!(usage_budget_reached_limits(&exhausted, (10, 5, 100)));
        }
    }
}
