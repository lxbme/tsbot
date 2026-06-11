use anyhow::Result;
use audiopus::coder::Encoder;
use audiopus::{Application, Channels, SampleRate};

use crate::frame::FRAME_SAMPLES;

/// 把一帧 48kHz 单声道 f32 PCM 编码为 Opus（音乐档）字节。
pub struct OpusMusicEncoder {
    enc: Encoder,
    buf: Vec<u8>,
}

impl OpusMusicEncoder {
    pub fn new() -> Result<Self> {
        let enc = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Audio)?;
        Ok(Self { enc, buf: vec![0u8; 4000] })
    }

    /// 编码恰好 `FRAME_SAMPLES` 个样本，返回编码后的字节切片。
    pub fn encode(&mut self, frame: &[f32; FRAME_SAMPLES]) -> Result<&[u8]> {
        let len = self.enc.encode_float(frame, &mut self.buf)?;
        Ok(&self.buf[..len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_silence_to_nonempty_opus() {
        let mut enc = OpusMusicEncoder::new().unwrap();
        let frame = [0f32; FRAME_SAMPLES];
        let out = enc.encode(&frame).unwrap();
        assert!(!out.is_empty());
    }
}
