use crate::mpv::player::MpvPlayer;

pub struct MediaInfoService;

impl MediaInfoService {
    pub fn get(mpv: &MpvPlayer) -> MediaInfo {
        MediaInfo {
            filename: mpv.get_property_string("filename").unwrap_or_default(),
            duration: mpv.get::<f64>("duration").unwrap_or(0.0),
            width: mpv.get_num("video-params/w", 0),
            height: mpv.get_num("video-params/h", 0),
            video_codec: mpv.get_property_string("video-codec").unwrap_or_default(),
            audio_codec: mpv.get_property_string("audio-codec-name").unwrap_or_default(),
            video_bitrate: mpv.get_num("video-bitrate", 0),
            audio_bitrate: mpv.get_num("audio-bitrate", 0),
            fps: mpv.get_num("container-fps", 0.0),
            file_size: mpv.get_num("file-size", 0),
        }
    }
}

pub struct MediaInfo {
    pub filename: String,
    pub duration: f64,
    pub width: i64,
    pub height: i64,
    pub video_codec: String,
    pub audio_codec: String,
    pub video_bitrate: i64,
    pub audio_bitrate: i64,
    pub fps: f64,
    pub file_size: i64,
}
