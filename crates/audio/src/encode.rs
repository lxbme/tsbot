use anyhow::Result;
use audiopus::coder::Encoder;
use audiopus::{Application, Bitrate, Channels, SampleRate, Signal};

use crate::frame::FRAME_SAMPLES;

/// 把一帧 48kHz 立体声 f32 PCM（L,R 交错）编码为 Opus（音乐档）字节。
pub struct OpusMusicEncoder {
    enc: Encoder,
    buf: Vec<u8>,
}

impl OpusMusicEncoder {
    pub fn new() -> Result<Self> {
        let mut enc = Encoder::new(SampleRate::Hz48000, Channels::Stereo, Application::Audio)?;
        // 显式设较高码率 + 音乐信号，保证音质（默认 auto 约 99k 偏低）。
        enc.set_bitrate(Bitrate::BitsPerSecond(128_000))?;
        enc.set_signal(Signal::Music)?;
        Ok(Self { enc, buf: vec![0u8; 4000] })
    }

    /// 编码恰好 `FRAME_SAMPLES` 个交错样本，返回编码后的字节切片。
    pub fn encode(&mut self, frame: &[f32; FRAME_SAMPLES]) -> Result<&[u8]> {
        let len = self.enc.encode_float(frame, &mut self.buf)?;
        Ok(&self.buf[..len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use audiopus::coder::Decoder;

    #[test]
    fn encodes_silence_to_nonempty_opus() {
        let mut enc = OpusMusicEncoder::new().unwrap();
        let frame = [0f32; FRAME_SAMPLES];
        let out = enc.encode(&frame).unwrap();
        assert!(!out.is_empty());
    }

    /// 立体声端到端：左声道有信号、右声道静音，编解码后右声道应仍明显弱于左声道，
    /// 证明管线确实承载立体声（单声道下混会把两声道混成相同）。
    #[test]
    fn preserves_stereo_separation() {
        let mut enc = OpusMusicEncoder::new().unwrap();
        // 构造一帧：L = 1kHz 正弦，R = 0（交错 L,R,L,R…）
        let mut frame = [0f32; FRAME_SAMPLES];
        for i in 0..FRAME_SAMPLES / 2 {
            let t = i as f32 / 48000.0;
            frame[2 * i] = (2.0 * std::f32::consts::PI * 1000.0 * t).sin() * 0.5; // L
            frame[2 * i + 1] = 0.0; // R
        }
        let bytes = enc.encode(&frame).unwrap().to_vec();

        let mut dec = Decoder::new(SampleRate::Hz48000, Channels::Stereo).unwrap();
        let mut out = [0f32; FRAME_SAMPLES];
        let per_ch = dec.decode_float(Some(&bytes), &mut out[..], false).unwrap();
        assert_eq!(per_ch, FRAME_SAMPLES / 2);

        let mut l_energy = 0f64;
        let mut r_energy = 0f64;
        for i in 0..per_ch {
            l_energy += (out[2 * i] as f64).powi(2);
            r_energy += (out[2 * i + 1] as f64).powi(2);
        }
        // 左有能量；右远小于左（立体声分离被保留）
        assert!(l_energy > 1.0, "left channel should carry signal: {l_energy}");
        assert!(r_energy < l_energy * 0.2, "right should stay much quieter: L={l_energy} R={r_energy}");
    }
}
