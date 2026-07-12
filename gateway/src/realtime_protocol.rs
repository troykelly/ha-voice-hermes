use std::fmt;
use std::str::FromStr;

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

pub const PROTOCOL_VERSION: u8 = 2;
pub const AUDIO_HEADER_LEN: usize = 20;
pub const MAX_AUDIO_PAYLOAD_BYTES: usize = 2_048;
pub const MAX_CONTROL_MESSAGE_BYTES: usize = 8 * 1024;
// Keep transcript controls safely below the 8 KiB control envelope even when
// every byte needs JSON escaping. The realtime gateway does not currently
// send transcripts to the headless Voice PE, but the protocol remains safe
// for future display clients.
pub const MAX_TRANSCRIPT_CHARS: usize = 3 * 1024;
pub const MAX_TRANSCRIPT_BYTES: usize = 3 * 1024;
pub const AUDIO_FORMAT: &str = "pcm_s16le_16000_mono";
pub const AUDIO_SAMPLE_RATE: u32 = 16_000;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TurnId(u64);

impl TurnId {
    pub const fn new(value: u64) -> Self {
        assert!(value != 0, "turn identifiers must be non-zero");
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for TurnId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:016x}", self.0)
    }
}

impl FromStr for TurnId {
    type Err = ProtocolError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 16
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ProtocolError::InvalidTurnId);
        }
        let parsed = u64::from_str_radix(value, 16).map_err(|_| ProtocolError::InvalidTurnId)?;
        if parsed == 0 {
            return Err(ProtocolError::InvalidTurnId);
        }
        Ok(Self(parsed))
    }
}

impl Serialize for TurnId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for TurnId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = <&str>::deserialize(deserializer)?;
        value.parse().map_err(de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum AudioKind {
    MicrophonePcm = 1,
    PlaybackPcm = 2,
}

impl TryFrom<u8> for AudioKind {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::MicrophonePcm),
            2 => Ok(Self::PlaybackPcm),
            _ => Err(ProtocolError::InvalidAudioKind),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AudioFlags(u8);

impl AudioFlags {
    pub const NONE: Self = Self(0);
    pub const END_OF_UTTERANCE: Self = Self(0x01);
    pub const DISCONTINUITY: Self = Self(0x02);
    pub const ALLOWED_BITS: u8 = Self::END_OF_UTTERANCE.0 | Self::DISCONTINUITY.0;

    pub fn new(bits: u8) -> Result<Self, ProtocolError> {
        if bits & !Self::ALLOWED_BITS != 0 {
            Err(ProtocolError::UnsupportedAudioFlags)
        } else {
            Ok(Self(bits))
        }
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn end_of_utterance(self) -> bool {
        self.0 & Self::END_OF_UTTERANCE.0 != 0
    }

    pub const fn discontinuity(self) -> bool {
        self.0 & Self::DISCONTINUITY.0 != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AudioHeader {
    pub kind: AudioKind,
    pub flags: AudioFlags,
    pub turn_id: TurnId,
    pub sequence: u32,
    pub first_sample: u32,
}

impl AudioHeader {
    pub fn encode(&self) -> [u8; AUDIO_HEADER_LEN] {
        let mut encoded = [0_u8; AUDIO_HEADER_LEN];
        encoded[0] = PROTOCOL_VERSION;
        encoded[1] = self.kind as u8;
        encoded[2] = self.flags.bits();
        encoded[3] = AUDIO_HEADER_LEN as u8;
        encoded[4..12].copy_from_slice(&self.turn_id.get().to_be_bytes());
        encoded[12..16].copy_from_slice(&self.sequence.to_be_bytes());
        encoded[16..20].copy_from_slice(&self.first_sample.to_be_bytes());
        encoded
    }

    pub fn decode(encoded: &[u8]) -> Result<Self, ProtocolError> {
        if encoded.len() != AUDIO_HEADER_LEN {
            return Err(ProtocolError::InvalidAudioHeaderLength);
        }
        if encoded[0] != PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedVersion);
        }
        if encoded[3] as usize != AUDIO_HEADER_LEN {
            return Err(ProtocolError::InvalidAudioHeaderLength);
        }

        let turn_id = u64::from_be_bytes(
            encoded[4..12]
                .try_into()
                .expect("fixed-width turn identifier"),
        );
        if turn_id == 0 {
            return Err(ProtocolError::InvalidTurnId);
        }

        Ok(Self {
            kind: encoded[1].try_into()?,
            flags: AudioFlags::new(encoded[2])?,
            turn_id: TurnId::new(turn_id),
            sequence: u32::from_be_bytes(
                encoded[12..16]
                    .try_into()
                    .expect("fixed-width sequence number"),
            ),
            first_sample: u32::from_be_bytes(
                encoded[16..20]
                    .try_into()
                    .expect("fixed-width sample offset"),
            ),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AudioFrame<'a> {
    pub header: AudioHeader,
    pub pcm: &'a [u8],
}

pub fn decode_audio_frame(
    encoded: &[u8],
    max_payload_bytes: usize,
) -> Result<AudioFrame<'_>, ProtocolError> {
    if encoded.len() < AUDIO_HEADER_LEN {
        return Err(ProtocolError::AudioFrameTooShort);
    }
    let header = AudioHeader::decode(&encoded[..AUDIO_HEADER_LEN])?;
    let pcm = &encoded[AUDIO_HEADER_LEN..];
    validate_pcm_payload(pcm, max_payload_bytes)?;
    Ok(AudioFrame { header, pcm })
}

pub fn encode_audio_frame(
    header: &AudioHeader,
    pcm: &[u8],
    max_payload_bytes: usize,
) -> Result<Vec<u8>, ProtocolError> {
    validate_pcm_payload(pcm, max_payload_bytes)?;
    let capacity = AUDIO_HEADER_LEN
        .checked_add(pcm.len())
        .ok_or(ProtocolError::AudioPayloadTooLarge)?;
    let mut encoded = Vec::with_capacity(capacity);
    encoded.extend_from_slice(&header.encode());
    encoded.extend_from_slice(pcm);
    Ok(encoded)
}

fn validate_pcm_payload(pcm: &[u8], max_payload_bytes: usize) -> Result<(), ProtocolError> {
    if pcm.is_empty() {
        return Err(ProtocolError::EmptyAudioPayload);
    }
    if !pcm.len().is_multiple_of(2) {
        return Err(ProtocolError::OddAudioPayload);
    }
    if pcm.len() > max_payload_bytes {
        return Err(ProtocolError::AudioPayloadTooLarge);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlDirection {
    DeviceToGateway,
    GatewayToDevice,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ControlMessage {
    #[serde(rename = "hello")]
    Hello {
        v: u8,
        firmware: String,
        input: String,
        output: String,
        barge_in: bool,
    },
    #[serde(rename = "ready")]
    Ready {
        v: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_window: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_window: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frame_ms: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_format: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_format: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conversation_id: Option<String>,
    },
    #[serde(rename = "conversation.reset")]
    ConversationReset { v: u8, request_id: String },
    #[serde(rename = "conversation.reset.done")]
    ConversationResetDone {
        v: u8,
        request_id: String,
        conversation_id: String,
    },
    #[serde(rename = "turn.start")]
    TurnStart {
        v: u8,
        turn_id: TurnId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    #[serde(rename = "turn.ready")]
    TurnReady { v: u8, turn_id: TurnId },
    #[serde(rename = "turn.commit")]
    TurnCommit {
        v: u8,
        turn_id: TurnId,
        last_seq: u32,
    },
    #[serde(rename = "turn.cancel")]
    TurnCancel {
        v: u8,
        turn_id: TurnId,
        reason: String,
    },
    #[serde(rename = "input.ack")]
    InputAck { v: u8, turn_id: TurnId, seq: u32 },
    #[serde(rename = "output.ack")]
    OutputAck { v: u8, turn_id: TurnId, seq: u32 },
    #[serde(rename = "transcript.partial")]
    TranscriptPartial {
        v: u8,
        turn_id: TurnId,
        text: String,
    },
    #[serde(rename = "transcript.final")]
    TranscriptFinal {
        v: u8,
        turn_id: TurnId,
        text: String,
    },
    #[serde(rename = "response.start")]
    ResponseStart { v: u8, turn_id: TurnId },
    #[serde(rename = "tts.start")]
    TtsStart {
        v: u8,
        turn_id: TurnId,
        sample_rate: u32,
    },
    #[serde(rename = "tts.end")]
    TtsEnd {
        v: u8,
        turn_id: TurnId,
        last_seq: u32,
    },
    #[serde(rename = "turn.done")]
    TurnDone {
        v: u8,
        turn_id: TurnId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cancelled: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    #[serde(rename = "error")]
    Error {
        v: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<TurnId>,
        code: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fatal: Option<bool>,
    },
    #[serde(rename = "ping")]
    Ping { v: u8 },
    #[serde(rename = "pong")]
    Pong { v: u8 },
}

impl ControlMessage {
    pub fn message_type(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::Ready { .. } => "ready",
            Self::ConversationReset { .. } => "conversation.reset",
            Self::ConversationResetDone { .. } => "conversation.reset.done",
            Self::TurnStart { .. } => "turn.start",
            Self::TurnReady { .. } => "turn.ready",
            Self::TurnCommit { .. } => "turn.commit",
            Self::TurnCancel { .. } => "turn.cancel",
            Self::InputAck { .. } => "input.ack",
            Self::OutputAck { .. } => "output.ack",
            Self::TranscriptPartial { .. } => "transcript.partial",
            Self::TranscriptFinal { .. } => "transcript.final",
            Self::ResponseStart { .. } => "response.start",
            Self::TtsStart { .. } => "tts.start",
            Self::TtsEnd { .. } => "tts.end",
            Self::TurnDone { .. } => "turn.done",
            Self::Error { .. } => "error",
            Self::Ping { .. } => "ping",
            Self::Pong { .. } => "pong",
        }
    }

    pub fn turn_id(&self) -> Option<TurnId> {
        match self {
            Self::TurnStart { turn_id, .. }
            | Self::TurnReady { turn_id, .. }
            | Self::TurnCommit { turn_id, .. }
            | Self::TurnCancel { turn_id, .. }
            | Self::InputAck { turn_id, .. }
            | Self::OutputAck { turn_id, .. }
            | Self::TranscriptPartial { turn_id, .. }
            | Self::TranscriptFinal { turn_id, .. }
            | Self::ResponseStart { turn_id, .. }
            | Self::TtsStart { turn_id, .. }
            | Self::TtsEnd { turn_id, .. }
            | Self::TurnDone { turn_id, .. } => Some(*turn_id),
            Self::Error { turn_id, .. } => *turn_id,
            Self::Hello { .. }
            | Self::Ready { .. }
            | Self::ConversationReset { .. }
            | Self::ConversationResetDone { .. }
            | Self::Ping { .. }
            | Self::Pong { .. } => None,
        }
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.version() != PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedVersion);
        }
        if self.turn_id().is_some_and(|turn_id| turn_id.get() == 0) {
            return Err(ProtocolError::InvalidTurnId);
        }

        match self {
            Self::Hello {
                firmware,
                input,
                output,
                ..
            } => {
                validate_text(firmware, 1, 128, "firmware")?;
                if input != AUDIO_FORMAT || output != AUDIO_FORMAT {
                    return Err(ProtocolError::InvalidControl("audio format"));
                }
            }
            Self::Ready {
                input_window,
                output_window,
                frame_ms,
                input_format,
                output_format,
                conversation_id,
                ..
            } => {
                for value in [input_window, output_window, frame_ms]
                    .into_iter()
                    .flatten()
                {
                    if *value == 0 {
                        return Err(ProtocolError::InvalidControl("ready bounds"));
                    }
                }
                for format in [input_format, output_format].into_iter().flatten() {
                    if format != AUDIO_FORMAT {
                        return Err(ProtocolError::InvalidControl("audio format"));
                    }
                }
                if let Some(conversation_id) = conversation_id {
                    validate_label(conversation_id, 64, "conversation id")?;
                }
            }
            Self::ConversationReset { request_id, .. } => {
                validate_fixed_hex_id(request_id, "reset request id")?;
            }
            Self::ConversationResetDone {
                request_id,
                conversation_id,
                ..
            } => {
                validate_fixed_hex_id(request_id, "reset request id")?;
                validate_label(conversation_id, 64, "conversation id")?;
            }
            Self::TurnStart {
                reason: Some(reason),
                ..
            } => validate_label(reason, 64, "start reason")?,
            Self::TurnCancel { reason, .. } => validate_label(reason, 64, "cancel reason")?,
            Self::TranscriptPartial { text, .. } | Self::TranscriptFinal { text, .. } => {
                validate_text(text, 1, MAX_TRANSCRIPT_CHARS, "transcript")?;
                if text.len() > MAX_TRANSCRIPT_BYTES {
                    return Err(ProtocolError::InvalidControl("transcript"));
                }
            }
            Self::TtsStart { sample_rate, .. } if *sample_rate != AUDIO_SAMPLE_RATE => {
                return Err(ProtocolError::InvalidControl("sample rate"));
            }
            Self::TurnDone {
                reason: Some(reason),
                ..
            } => validate_label(reason, 64, "done reason")?,
            Self::Error { code, message, .. } => {
                validate_label(code, 64, "error code")?;
                if let Some(message) = message {
                    validate_text(message, 1, 256, "error message")?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub fn validate_direction(&self, direction: ControlDirection) -> Result<(), ProtocolError> {
        let valid = match direction {
            ControlDirection::DeviceToGateway => matches!(
                self,
                Self::Hello { .. }
                    | Self::ConversationReset { .. }
                    | Self::TurnStart { .. }
                    | Self::TurnCommit { .. }
                    | Self::TurnCancel { .. }
                    | Self::OutputAck { .. }
                    | Self::Ping { .. }
                    | Self::Pong { .. }
            ),
            ControlDirection::GatewayToDevice => !matches!(
                self,
                Self::Hello { .. }
                    | Self::ConversationReset { .. }
                    | Self::TurnStart { .. }
                    | Self::TurnCommit { .. }
                    | Self::TurnCancel { .. }
                    | Self::OutputAck { .. }
            ),
        };
        if valid {
            Ok(())
        } else {
            Err(ProtocolError::InvalidControlDirection)
        }
    }

    fn version(&self) -> u8 {
        match self {
            Self::Hello { v, .. }
            | Self::Ready { v, .. }
            | Self::ConversationReset { v, .. }
            | Self::ConversationResetDone { v, .. }
            | Self::TurnStart { v, .. }
            | Self::TurnReady { v, .. }
            | Self::TurnCommit { v, .. }
            | Self::TurnCancel { v, .. }
            | Self::InputAck { v, .. }
            | Self::OutputAck { v, .. }
            | Self::TranscriptPartial { v, .. }
            | Self::TranscriptFinal { v, .. }
            | Self::ResponseStart { v, .. }
            | Self::TtsStart { v, .. }
            | Self::TtsEnd { v, .. }
            | Self::TurnDone { v, .. }
            | Self::Error { v, .. }
            | Self::Ping { v }
            | Self::Pong { v } => *v,
        }
    }
}

pub fn decode_control_message(
    encoded: &str,
    max_bytes: usize,
) -> Result<ControlMessage, ProtocolError> {
    if encoded.len() > max_bytes {
        return Err(ProtocolError::ControlMessageTooLarge);
    }
    let message: ControlMessage =
        serde_json::from_str(encoded).map_err(|_| ProtocolError::InvalidControlJson)?;
    message.validate()?;
    Ok(message)
}

pub fn encode_control_message(
    message: &ControlMessage,
    max_bytes: usize,
) -> Result<String, ProtocolError> {
    message.validate()?;
    let encoded = serde_json::to_string(message).map_err(|_| ProtocolError::InvalidControlJson)?;
    if encoded.len() > max_bytes {
        return Err(ProtocolError::ControlMessageTooLarge);
    }
    Ok(encoded)
}

fn validate_label(value: &str, max_chars: usize, field: &'static str) -> Result<(), ProtocolError> {
    let length = value.chars().count();
    if length == 0
        || length > max_chars
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(ProtocolError::InvalidControl(field));
    }
    Ok(())
}

fn validate_fixed_hex_id(value: &str, field: &'static str) -> Result<(), ProtocolError> {
    if value.len() != 16
        || value == "0000000000000000"
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ProtocolError::InvalidControl(field));
    }
    Ok(())
}

fn validate_text(
    value: &str,
    min_chars: usize,
    max_chars: usize,
    field: &'static str,
) -> Result<(), ProtocolError> {
    let length = value.chars().count();
    if value.trim().is_empty()
        || length < min_chars
        || length > max_chars
        || value.chars().any(|character| {
            character == '\0' || (character.is_control() && !character.is_whitespace())
        })
    {
        return Err(ProtocolError::InvalidControl(field));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    AudioFrameTooShort,
    InvalidAudioHeaderLength,
    UnsupportedVersion,
    InvalidAudioKind,
    UnsupportedAudioFlags,
    EmptyAudioPayload,
    OddAudioPayload,
    AudioPayloadTooLarge,
    InvalidTurnId,
    ControlMessageTooLarge,
    InvalidControlJson,
    InvalidControl(&'static str),
    InvalidControlDirection,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::AudioFrameTooShort => "audio frame is shorter than its header",
            Self::InvalidAudioHeaderLength => "audio header length is invalid",
            Self::UnsupportedVersion => "protocol version is unsupported",
            Self::InvalidAudioKind => "audio frame kind is invalid",
            Self::UnsupportedAudioFlags => "audio flags are unsupported",
            Self::EmptyAudioPayload => "audio payload is empty",
            Self::OddAudioPayload => "PCM16 audio payload length is odd",
            Self::AudioPayloadTooLarge => "audio payload exceeds its bound",
            Self::InvalidTurnId => "turn identifier is invalid",
            Self::ControlMessageTooLarge => "control message exceeds its bound",
            Self::InvalidControlJson => "control message JSON is invalid",
            Self::InvalidControl(field) => {
                return write!(formatter, "control field is invalid: {field}")
            }
            Self::InvalidControlDirection => "control message direction is invalid",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for ProtocolError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(kind: AudioKind) -> AudioHeader {
        AudioHeader {
            kind,
            flags: AudioFlags::NONE,
            turn_id: TurnId::new(0x0123_4567_89ab_cdef),
            sequence: 0x1020_3040,
            first_sample: 0x5060_7080,
        }
    }

    #[test]
    fn audio_header_has_exact_network_byte_order_layout() {
        assert_eq!(
            header(AudioKind::MicrophonePcm).encode(),
            [
                2, 1, 0, 20, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x10, 0x20, 0x30,
                0x40, 0x50, 0x60, 0x70, 0x80,
            ]
        );
    }

    #[test]
    fn audio_header_round_trips_both_directions() {
        for kind in [AudioKind::MicrophonePcm, AudioKind::PlaybackPcm] {
            let original = header(kind);
            assert_eq!(AudioHeader::decode(&original.encode()), Ok(original));
        }
    }

    #[test]
    fn exposes_the_two_v2_audio_hints_and_rejects_reserved_bits() {
        let flags = AudioFlags::new(0x03).unwrap();
        assert!(flags.end_of_utterance());
        assert!(flags.discontinuity());
        assert_eq!(flags.bits(), 0x03);
        assert_eq!(
            AudioFlags::new(0x04),
            Err(ProtocolError::UnsupportedAudioFlags)
        );
    }

    #[test]
    fn rejects_wrong_version_kind_header_length_and_reserved_flags() {
        let valid = header(AudioKind::MicrophonePcm).encode();
        for (index, replacement, expected) in [
            (0, 1, ProtocolError::UnsupportedVersion),
            (1, 9, ProtocolError::InvalidAudioKind),
            (2, 4, ProtocolError::UnsupportedAudioFlags),
            (3, 19, ProtocolError::InvalidAudioHeaderLength),
        ] {
            let mut malformed = valid;
            malformed[index] = replacement;
            assert_eq!(AudioHeader::decode(&malformed), Err(expected));
        }
        assert_eq!(
            AudioHeader::decode(&valid[..19]),
            Err(ProtocolError::InvalidAudioHeaderLength)
        );
        let mut zero_turn = valid;
        zero_turn[4..12].fill(0);
        assert_eq!(
            AudioHeader::decode(&zero_turn),
            Err(ProtocolError::InvalidTurnId)
        );
    }

    #[test]
    fn frame_round_trip_borrows_pcm_without_reinterpreting_little_endian_samples() {
        let pcm = [0x34, 0x12, 0xfe, 0xff];
        let original_header = header(AudioKind::PlaybackPcm);
        let encoded = encode_audio_frame(&original_header, &pcm, 4).unwrap();
        let decoded = decode_audio_frame(&encoded, 4).unwrap();
        assert_eq!(decoded.header, original_header);
        assert_eq!(decoded.pcm, pcm);
    }

    #[test]
    fn enforces_pcm_payload_bounds_and_alignment() {
        let encoded_header = header(AudioKind::MicrophonePcm).encode();
        assert_eq!(
            decode_audio_frame(&encoded_header, 8),
            Err(ProtocolError::EmptyAudioPayload)
        );
        let mut odd = encoded_header.to_vec();
        odd.push(0);
        assert_eq!(
            decode_audio_frame(&odd, 8),
            Err(ProtocolError::OddAudioPayload)
        );
        let mut large = encoded_header.to_vec();
        large.extend_from_slice(&[0; 10]);
        assert_eq!(
            decode_audio_frame(&large, 8),
            Err(ProtocolError::AudioPayloadTooLarge)
        );

        let mut device_oversized = encoded_header.to_vec();
        device_oversized.extend_from_slice(&vec![0; MAX_AUDIO_PAYLOAD_BYTES + 2]);
        assert_eq!(
            decode_audio_frame(&device_oversized, MAX_AUDIO_PAYLOAD_BYTES),
            Err(ProtocolError::AudioPayloadTooLarge)
        );
    }

    #[test]
    fn turn_id_is_fixed_width_hex_in_display_parse_and_json() {
        let turn_id = TurnId::new(0x0123_4567_89ab_cdef);
        assert_eq!(turn_id.to_string(), "0123456789abcdef");
        assert_eq!(
            serde_json::to_string(&turn_id).unwrap(),
            "\"0123456789abcdef\""
        );
        assert_eq!(
            serde_json::from_str::<TurnId>("\"0123456789abcdef\"").unwrap(),
            turn_id
        );
        for invalid in [
            "1",
            "0000000000000000",
            "00000000000000000",
            "00000000000000xz",
            "+000000000000001",
            "0123456789ABCDEF",
        ] {
            assert_eq!(invalid.parse::<TurnId>(), Err(ProtocolError::InvalidTurnId));
        }
        assert!(std::panic::catch_unwind(|| TurnId::new(0)).is_err());
    }

    #[test]
    fn decodes_and_validates_client_controls() {
        let hello = decode_control_message(
            r#"{"v":2,"type":"hello","firmware":"ha-voice-hermes/0.3.0","input":"pcm_s16le_16000_mono","output":"pcm_s16le_16000_mono","barge_in":true}"#,
            MAX_CONTROL_MESSAGE_BYTES,
        )
        .unwrap();
        assert_eq!(hello.message_type(), "hello");
        assert_eq!(hello.turn_id(), None);
        assert_eq!(
            hello.validate_direction(ControlDirection::DeviceToGateway),
            Ok(())
        );

        let commit = decode_control_message(
            r#"{"v":2,"type":"turn.commit","turn_id":"0000000000000042","last_seq":17}"#,
            MAX_CONTROL_MESSAGE_BYTES,
        )
        .unwrap();
        assert_eq!(commit.turn_id(), Some(TurnId::new(0x42)));
        assert_eq!(
            commit.validate_direction(ControlDirection::DeviceToGateway),
            Ok(())
        );
    }

    #[test]
    fn all_control_type_names_serialize_exactly() {
        let turn_id = TurnId::new(0x42);
        let messages = [
            ControlMessage::Ready {
                v: 2,
                input_window: None,
                output_window: None,
                frame_ms: None,
                input_format: None,
                output_format: None,
                conversation_id: Some("c00000001".into()),
            },
            ControlMessage::ConversationReset {
                v: 2,
                request_id: "0123456789abcdef".into(),
            },
            ControlMessage::ConversationResetDone {
                v: 2,
                request_id: "0123456789abcdef".into(),
                conversation_id: "c00000002".into(),
            },
            ControlMessage::TurnStart {
                v: 2,
                turn_id,
                reason: Some("wake_word".into()),
            },
            ControlMessage::TurnReady { v: 2, turn_id },
            ControlMessage::InputAck {
                v: 2,
                turn_id,
                seq: 1,
            },
            ControlMessage::OutputAck {
                v: 2,
                turn_id,
                seq: 1,
            },
            ControlMessage::TranscriptPartial {
                v: 2,
                turn_id,
                text: "partial".into(),
            },
            ControlMessage::TranscriptFinal {
                v: 2,
                turn_id,
                text: "final".into(),
            },
            ControlMessage::ResponseStart { v: 2, turn_id },
            ControlMessage::TtsStart {
                v: 2,
                turn_id,
                sample_rate: 16_000,
            },
            ControlMessage::TtsEnd {
                v: 2,
                turn_id,
                last_seq: 2,
            },
            ControlMessage::TurnDone {
                v: 2,
                turn_id,
                cancelled: None,
                reason: None,
            },
            ControlMessage::Error {
                v: 2,
                turn_id: Some(turn_id),
                code: "tts_failed".into(),
                message: None,
                fatal: Some(false),
            },
            ControlMessage::Ping { v: 2 },
            ControlMessage::Pong { v: 2 },
        ];
        let expected = [
            "ready",
            "conversation.reset",
            "conversation.reset.done",
            "turn.start",
            "turn.ready",
            "input.ack",
            "output.ack",
            "transcript.partial",
            "transcript.final",
            "response.start",
            "tts.start",
            "tts.end",
            "turn.done",
            "error",
            "ping",
            "pong",
        ];
        for (message, expected_type) in messages.iter().zip(expected) {
            let encoded = encode_control_message(message, MAX_CONTROL_MESSAGE_BYTES).unwrap();
            let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
            assert_eq!(value["type"], expected_type);
        }
    }

    #[test]
    fn rejects_unknown_fields_types_versions_formats_and_oversized_controls() {
        for invalid in [
            r#"{"v":3,"type":"ping"}"#,
            r#"{"v":2,"type":"future.message"}"#,
            r#"{"v":2,"type":"ping","unexpected":true}"#,
            r#"{"v":2,"type":"tts.start","turn_id":"0000000000000042","sample_rate":8000}"#,
            r#"{"v":2,"type":"turn.cancel","turn_id":"0000000000000042","reason":"not valid"}"#,
            r#"{"v":2,"type":"transcript.final","turn_id":"0000000000000042","text":"   "}"#,
            r#"{"v":2,"type":"conversation.reset","request_id":"ABC"}"#,
            r#"{"v":2,"type":"conversation.reset","request_id":"0000000000000000"}"#,
            r#"{"v":2,"type":"conversation.reset.done","request_id":"0123456789abcdef","conversation_id":"contains/slash"}"#,
        ] {
            assert!(decode_control_message(invalid, MAX_CONTROL_MESSAGE_BYTES).is_err());
        }
        assert_eq!(
            decode_control_message(r#"{"v":2,"type":"ping"}"#, 8),
            Err(ProtocolError::ControlMessageTooLarge)
        );
    }

    #[test]
    fn transcript_controls_have_independent_utf8_byte_and_character_bounds() {
        let turn_id = TurnId::new(1);
        let valid = ControlMessage::TranscriptFinal {
            v: PROTOCOL_VERSION,
            turn_id,
            text: "x".repeat(MAX_TRANSCRIPT_BYTES),
        };
        assert!(valid.validate().is_ok());

        let byte_oversized = ControlMessage::TranscriptFinal {
            v: PROTOCOL_VERSION,
            turn_id,
            text: "é".repeat(MAX_TRANSCRIPT_BYTES / 2 + 1),
        };
        assert_eq!(
            byte_oversized.validate(),
            Err(ProtocolError::InvalidControl("transcript"))
        );
    }

    #[test]
    fn rejects_controls_in_the_wrong_direction() {
        let server_message = ControlMessage::TranscriptFinal {
            v: 2,
            turn_id: TurnId::new(1),
            text: "hello".into(),
        };
        assert_eq!(
            server_message.validate_direction(ControlDirection::DeviceToGateway),
            Err(ProtocolError::InvalidControlDirection)
        );
        let client_message = ControlMessage::TurnStart {
            v: 2,
            turn_id: TurnId::new(1),
            reason: None,
        };
        assert_eq!(
            client_message.validate_direction(ControlDirection::GatewayToDevice),
            Err(ProtocolError::InvalidControlDirection)
        );

        let reset = ControlMessage::ConversationReset {
            v: 2,
            request_id: "0123456789abcdef".into(),
        };
        assert_eq!(
            reset.validate_direction(ControlDirection::DeviceToGateway),
            Ok(())
        );
        assert_eq!(
            reset.validate_direction(ControlDirection::GatewayToDevice),
            Err(ProtocolError::InvalidControlDirection)
        );
    }
}
