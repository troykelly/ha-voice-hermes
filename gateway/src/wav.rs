use std::fmt;

const EXPECTED_CHANNELS: u16 = 1;
const EXPECTED_SAMPLE_RATE: u32 = 16_000;
const EXPECTED_BITS_PER_SAMPLE: u16 = 16;
const EXPECTED_BLOCK_ALIGN: u16 = 2;
const EXPECTED_BYTE_RATE: u32 = 32_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WavInfo {
    pub data_bytes: u32,
    pub duration_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WavError {
    TooShort,
    InvalidRiff,
    InvalidRiffSize,
    TruncatedChunk,
    MissingFormat,
    MissingAudio,
    DuplicateAudio,
    UnsupportedEncoding,
    UnsupportedChannels,
    UnsupportedSampleRate,
    UnsupportedBitDepth,
    InvalidFormatRates,
    InvalidAudioLength,
    DurationExceeded,
}

impl fmt::Display for WavError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooShort => "WAV header is incomplete",
            Self::InvalidRiff => "audio is not a RIFF/WAVE file",
            Self::InvalidRiffSize => "RIFF size does not match the payload",
            Self::TruncatedChunk => "WAV contains a truncated chunk",
            Self::MissingFormat => "WAV is missing its fmt chunk",
            Self::MissingAudio => "WAV is missing audio data",
            Self::DuplicateAudio => "WAV contains multiple audio data chunks",
            Self::UnsupportedEncoding => "WAV must contain uncompressed PCM",
            Self::UnsupportedChannels => "WAV must be mono",
            Self::UnsupportedSampleRate => "WAV must use a 16000 Hz sample rate",
            Self::UnsupportedBitDepth => "WAV must use 16-bit samples",
            Self::InvalidFormatRates => "WAV byte rate or block alignment is invalid",
            Self::InvalidAudioLength => "WAV audio data length is invalid",
            Self::DurationExceeded => "WAV duration exceeds the configured limit",
        })
    }
}

pub fn validate_wav(bytes: &[u8], max_seconds: u32) -> Result<WavInfo, WavError> {
    if bytes.len() < 12 {
        return Err(WavError::TooShort);
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(WavError::InvalidRiff);
    }

    let declared_riff_size = read_u32(bytes, 4).ok_or(WavError::TooShort)? as usize;
    if declared_riff_size.checked_add(8) != Some(bytes.len()) {
        return Err(WavError::InvalidRiffSize);
    }

    let mut offset = 12_usize;
    let mut format: Option<(u16, u16, u32, u32, u16, u16)> = None;
    let mut data_size: Option<u32> = None;

    while offset < bytes.len() {
        let header_end = offset.checked_add(8).ok_or(WavError::TruncatedChunk)?;
        if header_end > bytes.len() {
            return Err(WavError::TruncatedChunk);
        }

        let chunk_id = &bytes[offset..offset + 4];
        let chunk_size = read_u32(bytes, offset + 4).ok_or(WavError::TruncatedChunk)? as usize;
        let data_start = header_end;
        let data_end = data_start
            .checked_add(chunk_size)
            .ok_or(WavError::TruncatedChunk)?;
        let padded_end = data_end
            .checked_add(chunk_size & 1)
            .ok_or(WavError::TruncatedChunk)?;
        if padded_end > bytes.len() {
            return Err(WavError::TruncatedChunk);
        }

        match chunk_id {
            b"fmt " if format.is_none() => {
                if chunk_size < 16 {
                    return Err(WavError::TruncatedChunk);
                }
                format = Some((
                    read_u16(bytes, data_start).ok_or(WavError::TruncatedChunk)?,
                    read_u16(bytes, data_start + 2).ok_or(WavError::TruncatedChunk)?,
                    read_u32(bytes, data_start + 4).ok_or(WavError::TruncatedChunk)?,
                    read_u32(bytes, data_start + 8).ok_or(WavError::TruncatedChunk)?,
                    read_u16(bytes, data_start + 12).ok_or(WavError::TruncatedChunk)?,
                    read_u16(bytes, data_start + 14).ok_or(WavError::TruncatedChunk)?,
                ));
            }
            b"data" => {
                if data_size.is_some() {
                    return Err(WavError::DuplicateAudio);
                }
                data_size = Some(chunk_size as u32);
            }
            _ => {}
        }

        offset = padded_end;
    }

    let (encoding, channels, sample_rate, byte_rate, block_align, bits_per_sample) =
        format.ok_or(WavError::MissingFormat)?;
    if encoding != 1 {
        return Err(WavError::UnsupportedEncoding);
    }
    if channels != EXPECTED_CHANNELS {
        return Err(WavError::UnsupportedChannels);
    }
    if sample_rate != EXPECTED_SAMPLE_RATE {
        return Err(WavError::UnsupportedSampleRate);
    }
    if bits_per_sample != EXPECTED_BITS_PER_SAMPLE {
        return Err(WavError::UnsupportedBitDepth);
    }
    if block_align != EXPECTED_BLOCK_ALIGN || byte_rate != EXPECTED_BYTE_RATE {
        return Err(WavError::InvalidFormatRates);
    }

    let data_bytes = data_size.ok_or(WavError::MissingAudio)?;
    if data_bytes == 0 || data_bytes % u32::from(block_align) != 0 {
        return Err(WavError::InvalidAudioLength);
    }
    if u64::from(data_bytes) > u64::from(byte_rate) * u64::from(max_seconds) {
        return Err(WavError::DurationExceeded);
    }

    Ok(WavInfo {
        data_bytes,
        duration_ms: u64::from(data_bytes) * 1_000 / u64::from(byte_rate),
    })
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let value = bytes.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_le_bytes([value[0], value[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let value = bytes.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav_with_format(channels: u16, sample_rate: u32, bits: u16, samples: usize) -> Vec<u8> {
        let block_align = channels * (bits / 8);
        let byte_rate = sample_rate * u32::from(block_align);
        let data_len = samples * usize::from(block_align);
        let total_len = 44 + data_len;
        let mut wav = Vec::with_capacity(total_len);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&((total_len - 8) as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&byte_rate.to_le_bytes());
        wav.extend_from_slice(&block_align.to_le_bytes());
        wav.extend_from_slice(&bits.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data_len as u32).to_le_bytes());
        wav.resize(total_len, 0);
        wav
    }

    #[test]
    fn accepts_standard_pcm16_mono_16khz() {
        let wav = wav_with_format(1, 16_000, 16, 16_000);
        let info = validate_wav(&wav, 2).expect("valid WAV");
        assert_eq!(info.data_bytes, 32_000);
        assert_eq!(info.duration_ms, 1_000);
    }

    #[test]
    fn rejects_stereo_audio() {
        let wav = wav_with_format(2, 16_000, 16, 16_000);
        assert_eq!(validate_wav(&wav, 2), Err(WavError::UnsupportedChannels));
    }

    #[test]
    fn rejects_truncated_chunks() {
        let mut wav = wav_with_format(1, 16_000, 16, 100);
        wav.truncate(wav.len() - 1);
        let new_size = (wav.len() - 8) as u32;
        wav[4..8].copy_from_slice(&new_size.to_le_bytes());
        assert_eq!(validate_wav(&wav, 2), Err(WavError::TruncatedChunk));
    }

    #[test]
    fn rejects_audio_over_duration_limit() {
        let wav = wav_with_format(1, 16_000, 16, 32_001);
        assert_eq!(validate_wav(&wav, 2), Err(WavError::DurationExceeded));
    }
}
