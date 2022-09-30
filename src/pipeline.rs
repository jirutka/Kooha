use anyhow::{ensure, Context, Ok, Result};
use gst::prelude::*;
use gtk::{glib, graphene::Rect};

use std::{
    ffi::OsStr,
    os::unix::io::RawFd,
    path::{Path, PathBuf},
};

use crate::{
    area_selector::Data as SelectAreaData, profile::Profile, screencast_session::Stream, utils,
};

// TODO
// * Do we need restrictions?
// * Can we drop filter elements (videorate, videoconvert, videoscale, audioconvert) and let encodebin handle it?
// * Can we set frame rate directly on profile format?
// * Add tests

const DEFAULT_AUDIO_SAMPLE_RATE: i32 = 44_100;

#[derive(Debug)]
#[must_use]
pub struct PipelineBuilder {
    saving_location: PathBuf,
    framerate: u32,
    profile: Box<dyn Profile>,
    fd: RawFd,
    streams: Vec<Stream>,
    speaker_source: Option<String>,
    mic_source: Option<String>,
    select_area_data: Option<SelectAreaData>,
}

impl PipelineBuilder {
    pub fn new(
        saving_location: &Path,
        framerate: u32,
        profile: Box<dyn Profile>,
        fd: RawFd,
        streams: Vec<Stream>,
    ) -> Self {
        Self {
            saving_location: saving_location.to_path_buf(),
            framerate,
            profile,
            fd,
            streams,
            speaker_source: None,
            mic_source: None,
            select_area_data: None,
        }
    }

    pub fn speaker_source(&mut self, speaker_source: String) -> &mut Self {
        self.speaker_source = Some(speaker_source);
        self
    }

    pub fn mic_source(&mut self, mic_source: String) -> &mut Self {
        self.mic_source = Some(mic_source);
        self
    }

    pub fn select_area_data(&mut self, data: SelectAreaData) -> &mut Self {
        self.select_area_data = Some(data);
        self
    }

    pub fn build(&self) -> Result<gst::Pipeline> {
        let file_path = new_recording_path(&self.saving_location, self.profile.file_extension());

        let queue = utils::make_named_element("queue", "sinkqueue")?;
        let filesink = utils::make_named_element("filesink", "filesink")?;
        filesink.set_property(
            "location",
            file_path
                .to_str()
                .context("Could not convert file path to string")?,
        );

        let pipeline = gst::Pipeline::new(None);
        pipeline.add_many(&[&queue, &filesink])?;
        queue.link(&filesink)?;

        tracing::debug!(
            file_path = %file_path.display(),
            framerate = self.framerate,
            profile = ?self.profile,
            stream_len = self.streams.len(),
            streams = ?self.streams,
            speaker_source = ?self.speaker_source,
            mic_source = ?self.mic_source,
            select_area_data = ?self.select_area_data,
        );

        ensure!(!self.streams.is_empty(), "No streams provided");

        let videosrc_bin = pipewiresrc_bin(
            self.fd,
            &self.streams,
            self.framerate,
            self.select_area_data.as_ref(),
        )
        .context("Failed to create videosrc bin")?;

        pipeline.add(&videosrc_bin)?;

        let audiosrc_bin = if self.profile.supports_audio()
            && (self.speaker_source.is_some() || self.mic_source.is_some())
        {
            let audiosrc_bin = pulsesrc_bin(
                [&self.speaker_source, &self.mic_source]
                    .into_iter()
                    .filter_map(|s| s.as_deref()),
            )
            .context("Failed to create audiosrc bin")?;
            pipeline.add(&audiosrc_bin)?;

            Some(audiosrc_bin)
        } else {
            if self.speaker_source.is_some() || self.mic_source.is_some() {
                tracing::warn!(
                    "Selected profiles does not support audio, but audio sources are provided. Ignoring"
                );
            }

            None
        };

        self.profile
            .attach(
                &pipeline,
                videosrc_bin.upcast_ref(),
                audiosrc_bin.as_ref().map(|a| a.upcast_ref()),
                &queue,
            )
            .context("Failed to attach profile to pipeline")?;

        Ok(pipeline)
    }
}

fn pipewiresrc_with_default(fd: RawFd, path: &str) -> Result<gst::Element> {
    let src = utils::make_element("pipewiresrc")?;
    src.set_property("fd", &fd);
    src.set_property("path", path);
    src.set_property("do-timestamp", true);
    src.set_property("keepalive-time", 1000);
    src.set_property("resend-last", true);

    // Workaround copied from https://gitlab.gnome.org/GNOME/gnome-shell/-/commit/d32c03488fcf6cdb0ca2e99b0ed6ade078460deb
    let needs_copy = gst::Registry::get().check_feature_version("pipewiresrc", 0, 3, 57);
    src.set_property("always-copy", needs_copy);

    Ok(src)
}

fn videoconvert_with_default() -> Result<gst::Element> {
    let conv = utils::make_element("videoconvert")?;
    conv.set_property("chroma-mode", gst_video::VideoChromaMode::None);
    conv.set_property("dither", gst_video::VideoDitherMethod::None);
    conv.set_property("matrix-mode", gst_video::VideoMatrixMode::OutputOnly);
    conv.set_property("n-threads", utils::ideal_thread_count());
    Ok(conv)
}

/// Create a videocrop element that computes the crop from the given coordinates
/// and size.
fn videocrop_compute(data: &SelectAreaData) -> Result<gst::Element> {
    let SelectAreaData {
        selection,
        paintable_rect,
        stream_size,
    } = data;

    let (stream_width, stream_height) = stream_size;
    let scale_factor_h = *stream_width as f32 / paintable_rect.width();
    let scale_factor_v = *stream_height as f32 / paintable_rect.height();

    if scale_factor_h != scale_factor_v {
        tracing::warn!(
            scale_factor_h,
            scale_factor_v,
            "Scale factors of horizontal and vertical are unequal"
        );
    }

    // Both paintable and selection position are relative to the widget coordinates.
    // To get the absolute position and so correct crop values, subtract the paintable
    // rect's position from the selection rect.
    let old_selection_rect = selection.rect();
    let selection_rect_scaled = Rect::new(
        old_selection_rect.x() - paintable_rect.x(),
        old_selection_rect.y() - paintable_rect.y(),
        old_selection_rect.width(),
        old_selection_rect.height(),
    )
    .scale(scale_factor_h, scale_factor_v);

    let raw_top_crop = selection_rect_scaled.y();
    let raw_left_crop = selection_rect_scaled.x();
    let raw_right_crop =
        *stream_width as f32 - (selection_rect_scaled.width() + selection_rect_scaled.x());
    let raw_bottom_crop =
        *stream_height as f32 - (selection_rect_scaled.height() + selection_rect_scaled.y());

    tracing::debug!(raw_top_crop, raw_left_crop, raw_right_crop, raw_bottom_crop);

    let top_crop = round_to_even_f32(raw_top_crop).clamp(0, *stream_height);
    let left_crop = round_to_even_f32(raw_left_crop).clamp(0, *stream_width);
    let right_crop = round_to_even_f32(raw_right_crop).clamp(0, *stream_width);
    let bottom_crop = round_to_even_f32(raw_bottom_crop).clamp(0, *stream_height);

    tracing::debug!(top_crop, left_crop, right_crop, bottom_crop);

    // x264enc requires even resolution.
    let crop = utils::make_element("videocrop")?;
    crop.set_property("top", top_crop);
    crop.set_property("left", left_crop);
    crop.set_property("right", right_crop);
    crop.set_property("bottom", bottom_crop);
    Ok(crop)
}

/// Creates a bin with a src pad for multiple pipewire streams.
///                                                                (If has select area data)
/// pipewiresrc1 -> videorate -> |                                       |            |
///                              |                                       V            V
/// pipewiresrc2 -> videorate -> | -> compositor -> videoconvert -> videoscale -> videocrop -> queue
///                              |
/// pipewiresrcn -> videorate -> |
pub fn pipewiresrc_bin(
    fd: RawFd,
    streams: &[Stream],
    framerate: u32,
    select_area_data: Option<&SelectAreaData>,
) -> Result<gst::Bin> {
    let bin = gst::Bin::new(None);

    let compositor = utils::make_element("compositor")?;
    let videoconvert = videoconvert_with_default()?;
    let queue = utils::make_element("queue")?;

    bin.add_many(&[&compositor, &videoconvert, &queue])?;
    compositor.link(&videoconvert)?;

    if let Some(data) = select_area_data {
        let videoscale = utils::make_element("videoscale")?;
        let videocrop = videocrop_compute(data)?;

        // x264enc requires even resolution.
        let (stream_width, stream_height) = data.stream_size;
        let videoscale_filter = gst::Caps::builder("video/x-raw")
            .field("width", round_to_even(stream_width))
            .field("height", round_to_even(stream_height))
            .build();

        bin.add_many(&[&videoscale, &videocrop])?;
        videoconvert.link(&videoscale)?;
        videoscale.link_filtered(&videocrop, &videoscale_filter)?;
        videocrop.link(&queue)?;
    } else {
        videoconvert.link(&queue)?;
    }

    let videorate_filter = gst::Caps::builder("video/x-raw")
        .field("framerate", gst::Fraction::new(framerate as i32, 1))
        .build();

    let mut last_pos = 0;
    for stream in streams {
        let pipewiresrc = pipewiresrc_with_default(fd, &stream.node_id().to_string())?;
        let videorate = utils::make_element("videorate")?;
        let videorate_capsfilter = utils::make_element("capsfilter")?;
        videorate_capsfilter.set_property("caps", &videorate_filter);

        bin.add_many(&[&pipewiresrc, &videorate, &videorate_capsfilter])?;
        gst::Element::link_many(&[&pipewiresrc, &videorate, &videorate_capsfilter])?;

        let compositor_sink_pad = compositor
            .request_pad_simple("sink_%u")
            .context("Failed to request sink_%u pad from compositor")?;
        compositor_sink_pad.set_property("xpos", last_pos);
        videorate_capsfilter
            .static_pad("src")
            .unwrap()
            .link(&compositor_sink_pad)?;

        let stream_width = stream.size().unwrap().0;
        last_pos += stream_width;
    }

    let queue_pad = queue.static_pad("src").unwrap();
    bin.add_pad(&gst::GhostPad::with_target(Some("src"), &queue_pad)?)?;

    Ok(bin)
}

/// Creates a bin with a src pad for a pulse audio device
///
/// pulsesrc1 -> audioresample -> audiorate -> |
///                                            |
/// pulsesrc2 -> audioresample -> audiorate -> | -> liveadder -> audioconvert -> queue
///                                            |
/// pulsesrcn -> audioresample -> audiorate -> |
fn pulsesrc_bin<'a>(device_names: impl IntoIterator<Item = &'a str>) -> Result<gst::Bin> {
    let bin = gst::Bin::new(None);

    let liveadder = utils::make_element("liveadder")?;
    let audioconvert = utils::make_element("audioconvert")?;
    let queue = utils::make_element("queue")?;

    let sample_rate_filter = gst::Caps::builder("audio/x-raw")
        .field("rate", DEFAULT_AUDIO_SAMPLE_RATE)
        .build();

    bin.add_many(&[&liveadder, &audioconvert, &queue])?;
    liveadder.link_filtered(&audioconvert, &sample_rate_filter)?;
    audioconvert.link(&queue)?;

    for device_name in device_names {
        let pulsesrc = utils::make_element("pulsesrc")?;
        pulsesrc.set_property("device", device_name);
        let audioresample = utils::make_element("audioresample")?;
        let audiorate = utils::make_element("audiorate")?;

        bin.add_many(&[&pulsesrc, &audioresample, &audiorate])?;
        pulsesrc.link(&audioresample)?;
        audioresample.link_filtered(&audiorate, &sample_rate_filter)?;

        let liveadder_sink_pad = liveadder
            .request_pad_simple("sink_%u")
            .context("Failed to request sink_%u pad from liveadder")?;
        audiorate
            .static_pad("src")
            .unwrap()
            .link(&liveadder_sink_pad)?;
    }

    let queue_pad = queue.static_pad("src").unwrap();
    bin.add_pad(&gst::GhostPad::with_target(Some("src"), &queue_pad)?)?;

    Ok(bin)
}

fn round_to_even(number: i32) -> i32 {
    number / 2 * 2
}

fn round_to_even_f32(number: f32) -> i32 {
    (number / 2.0).round() as i32 * 2
}

fn new_recording_path(saving_location: &Path, extension: impl AsRef<OsStr>) -> PathBuf {
    let file_name = glib::DateTime::now_local()
        .expect("You are somehow on year 9999")
        .format("Kooha-%F-%H-%M-%S")
        .expect("Invalid format string");

    let mut path = saving_location.join(file_name);
    path.set_extension(extension);

    path
}

#[cfg(test)]
mod test {
    use super::*;

    macro_rules! assert_even {
        ($number:expr) => {
            assert_eq!($number % 2, 0)
        };
    }

    #[test]
    fn odd_round_to_even() {
        assert_even!(round_to_even(5));
        assert_even!(round_to_even(101));
    }

    #[test]
    fn odd_round_to_even_f32() {
        assert_even!(round_to_even_f32(3.0));
        assert_even!(round_to_even_f32(99.0));
    }

    #[test]
    fn even_round_to_even() {
        assert_even!(round_to_even(50));
        assert_even!(round_to_even(4));
    }

    #[test]
    fn even_round_to_even_f32() {
        assert_even!(round_to_even_f32(300.0));
        assert_even!(round_to_even_f32(6.0));
    }

    #[test]
    fn float_round_to_even_f32() {
        assert_even!(round_to_even_f32(5.3));
        assert_even!(round_to_even_f32(2.9));
    }
}
