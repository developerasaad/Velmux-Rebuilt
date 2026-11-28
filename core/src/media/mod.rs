mod decoded;
mod ebml;
pub(crate) mod input;
mod inspect;

pub(crate) use inspect::{inspect, inspect_file};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct Rational {
    pub numerator: i32,
    pub denominator: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct MetadataEntry {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum StreamKind {
    Video,
    Audio,
    Subtitle,
    Data,
    Attachment,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
pub struct MasteringDisplayInfo {
    /// Red, green and blue x/y coordinates, in that order.
    pub primaries: Vec<Rational>,
    pub white_point: Vec<Rational>,
    pub min_luminance: Option<Rational>,
    pub max_luminance: Option<Rational>,
}

#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
pub struct ContentLightInfo {
    pub max_content_light_level: u32,
    pub max_frame_average_light_level: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
pub struct VideoInfo {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub sample_aspect_ratio: Option<Rational>,
    /// Aspect ratio before applying the separately reported display matrix.
    pub display_aspect_ratio: Option<Rational>,
    pub average_frame_rate: Option<Rational>,
    pub nominal_frame_rate: Option<Rational>,
    /// Container guesses and average rates alone do not establish VFR.
    pub variable_frame_rate: Option<bool>,
    pub rotation_degrees_counter_clockwise: Option<f64>,
    pub display_matrix: Vec<i32>,
    pub pixel_format: Option<String>,
    pub bit_depth: Option<u32>,
    pub color_primaries: Option<String>,
    pub color_transfer: Option<String>,
    pub color_space: Option<String>,
    pub color_range: Option<String>,
    pub chroma_location: Option<String>,
    pub attached_picture: bool,
    pub mastering_display: Option<MasteringDisplayInfo>,
    pub content_light: Option<ContentLightInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
pub struct AudioInfo {
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub channel_layout: Option<String>,
    pub sample_format: Option<String>,
    pub bit_depth: Option<u32>,
    pub initial_padding: u32,
    pub trailing_padding: u32,
    pub seek_preroll: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum SubtitleRepresentation {
    Text,
    Bitmap,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
pub struct SubtitleInfo {
    pub representation: SubtitleRepresentation,
}

#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
pub struct StreamInfo {
    /// Identifies a stream within this exact inspected source, never a UI row.
    pub id: String,
    pub index: u32,
    pub container_id: i32,
    pub kind: StreamKind,
    pub codec_id: i32,
    pub codec: String,
    pub codec_long_name: Option<String>,
    pub decoder_available: bool,
    pub time_base: Option<Rational>,
    pub duration_us: Option<i64>,
    pub start_time_us: Option<i64>,
    pub frame_count: Option<u64>,
    pub bit_rate: Option<u64>,
    pub disposition: u32,
    pub profile: Option<String>,
    pub level: Option<i32>,
    pub metadata: Vec<MetadataEntry>,
    pub video: Option<VideoInfo>,
    pub audio: Option<AudioInfo>,
    pub subtitle: Option<SubtitleInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
pub struct ChapterInfo {
    pub id: i64,
    pub start_time_us: Option<i64>,
    pub end_time_us: Option<i64>,
    pub metadata: Vec<MetadataEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
pub struct MediaInfo {
    /// Descriptor metadata plus bounded content samples; not a whole-file hash.
    pub source_identity: String,
    pub byte_size: Option<u64>,
    pub container: String,
    pub container_long_name: Option<String>,
    pub duration_us: Option<i64>,
    pub start_time_us: Option<i64>,
    pub bit_rate: Option<u64>,
    pub streams: Vec<StreamInfo>,
    pub metadata: Vec<MetadataEntry>,
    pub chapters: Vec<ChapterInfo>,
    pub staged: bool,
}
