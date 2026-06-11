mod encode;
mod frame;

pub use encode::OpusMusicEncoder;
pub use frame::{spawn_ffmpeg, PcmFrameReader, FRAME_SAMPLES};
