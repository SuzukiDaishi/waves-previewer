//! Cross-platform H.264 decoding, via the bundled OpenH264.
//!
//! OpenH264 decodes in **decode order**: hand it one access unit and it hands
//! back that access unit's picture, not the next one due on screen. With
//! B-frames those differ, so this backend feeds samples by id (decode order),
//! parks what comes out in a small map keyed by presentation time, and serves
//! the UI from that map in presentation order. The stream's own timestamps are
//! never consulted — `DecodedYUV::timestamp` reflects an input timestamp this
//! code never sets — so every frame's time comes from the container index.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use openh264::formats::YUVSource;

use super::annexb;
use super::container::{VideoCodec, VideoContainer, VideoStreamInfo};
use super::frame::{rgba_to_color_image, VideoFrame};
use super::VideoDecoder;

/// Most samples fed to the decoder in one `next_frame` call.
///
/// Bounds the walk from a keyframe to the requested picture. A GOP longer than
/// this means a seek lands on an approximate frame rather than freezing the
/// worker on a pathological file.
const MAX_SAMPLES_PER_STEP: usize = 900;

/// Most decoded-but-unshown frames held while walking a GOP.
const MAX_REORDER_FRAMES: usize = 64;

pub struct OpenH264Decoder {
    container: VideoContainer,
    decoder: openh264::decoder::Decoder,
    /// Next sample id to feed, in decode order (ids are 1-based and dense).
    next_decode_id: u32,
    /// Next index position to deliver, in presentation order.
    next_present_pos: usize,
    /// Decoded frames not yet delivered, keyed by presentation timestamp.
    reorder: BTreeMap<i64, Arc<egui::ColorImage>>,
    /// Scratch buffers reused across frames so a 30 fps panel does not
    /// allocate a megabyte per picture.
    annexb_scratch: Vec<u8>,
    rgba_scratch: Vec<u8>,
    /// The box size the frames in `reorder` were scaled into. A change
    /// invalidates them, because they were shrunk on the way in.
    reorder_box: (u32, u32),
}

impl OpenH264Decoder {
    pub fn open(container: VideoContainer) -> Result<Self> {
        if container.info.codec != VideoCodec::H264 {
            anyhow::bail!(
                "openh264 decodes H.264 only, this track is {}",
                container.info.codec_label
            );
        }
        if container.params.is_empty() {
            anyhow::bail!("H.264 track has no parameter sets in its avcC box");
        }
        let decoder = openh264::decoder::Decoder::new()
            .map_err(|e| anyhow::anyhow!("openh264 decoder init: {e}"))?;
        let first_id = container.index.entry(0).map(|entry| entry.id).unwrap_or(1);
        Ok(Self {
            container,
            decoder,
            next_decode_id: first_id,
            next_present_pos: 0,
            reorder: BTreeMap::new(),
            annexb_scratch: Vec::new(),
            rgba_scratch: Vec::new(),
            reorder_box: (0, 0),
        })
    }

    /// Drop decoder state and restart from the sync sample covering
    /// `present_pos`.
    ///
    /// OpenH264 has no reset entry point that is safe to use mid-stream, so a
    /// backwards seek builds a new decoder. That costs about a millisecond,
    /// which is cheap next to decoding a GOP.
    fn restart_at(&mut self, present_pos: usize) -> Result<()> {
        let sync_pos = self.container.index.sync_at_or_before(present_pos);
        let sync_id = self
            .container
            .index
            .entry(sync_pos)
            .map(|e| e.id)
            .unwrap_or(1);
        self.decoder = openh264::decoder::Decoder::new()
            .map_err(|e| anyhow::anyhow!("openh264 decoder restart: {e}"))?;
        self.next_decode_id = sync_id;
        self.next_present_pos = present_pos;
        self.reorder.clear();
        Ok(())
    }

    /// Feed one sample and park whatever picture comes back.
    fn feed_one(&mut self, box_px: (u32, u32)) -> Result<bool> {
        let id = self.next_decode_id;
        if id == 0 || (id as usize) > self.container.index.len() {
            return Ok(false);
        }
        // Sample ids are dense and 1-based, so id - 1 is its decode-order slot;
        // the index is presentation-sorted, so find the entry carrying this id.
        let position = self.container.index.position_of_id(id);
        let Some(position) = position else {
            self.next_decode_id = self.next_decode_id.saturating_add(1);
            return Ok(true);
        };
        let pts = self
            .container
            .index
            .entry(position)
            .map(|e| e.pts)
            .unwrap_or(0);
        let bytes = self.container.read_sample_at(position)?;
        self.next_decode_id = self.next_decode_id.saturating_add(1);
        let Some(bytes) = bytes else {
            return Ok(true);
        };
        let params = self.container.params.clone();
        if !annexb::sample_to_annexb(&bytes, &params, &mut self.annexb_scratch) {
            return Ok(true);
        }

        let rotation = self.container.info.rotation;
        let decoded = self.decoder.decode(&self.annexb_scratch);
        let Some(yuv) = decoded.map_err(|e| anyhow::anyhow!("h264 decode: {e}"))? else {
            return Ok(true);
        };
        let (w, h) = yuv.dimensions();
        if w == 0 || h == 0 {
            return Ok(true);
        }
        // `write_rgba8` asserts on a mismatched target, so size it from what
        // the decoder actually produced rather than from the container's
        // declared dimensions (they can disagree on a cropped stream).
        self.rgba_scratch.clear();
        self.rgba_scratch.resize(w * h * 4, 0);
        yuv.write_rgba8(&mut self.rgba_scratch);
        if let Some(image) = rgba_to_color_image(
            &self.rgba_scratch,
            w as u32,
            h as u32,
            rotation,
            box_px.0,
            box_px.1,
        ) {
            if self.reorder.len() >= MAX_REORDER_FRAMES {
                // Keep the newest: an over-long reorder run means the wanted
                // frame is still ahead, and the oldest entries are behind it.
                if let Some(oldest) = self.reorder.keys().next().copied() {
                    self.reorder.remove(&oldest);
                }
            }
            self.reorder.insert(pts, Arc::new(image));
        }
        Ok(true)
    }
}

impl VideoDecoder for OpenH264Decoder {
    fn info(&self) -> &VideoStreamInfo {
        &self.container.info
    }

    fn seek(&mut self, secs: f64, max_forward_walk: usize) -> Result<()> {
        let Some(target) = self.container.index.position_at_or_before(secs) else {
            return Ok(());
        };
        let can_walk_forward =
            target >= self.next_present_pos && target - self.next_present_pos <= max_forward_walk;
        if can_walk_forward {
            // Everything between here and the target still has to be decoded
            // (later frames reference it), but nothing needs to be shown, so
            // just move the delivery cursor and let the feed loop catch up.
            self.next_present_pos = target;
            let cutoff = self
                .container
                .index
                .entry(target)
                .map(|e| e.pts)
                .unwrap_or(i64::MIN);
            self.reorder.retain(|pts, _| *pts >= cutoff);
            return Ok(());
        }
        self.restart_at(target)
    }

    fn next_frame(
        &mut self,
        box_px: (u32, u32),
        cancel: &AtomicBool,
    ) -> Result<Option<VideoFrame>> {
        if self.next_present_pos >= self.container.index.len() {
            return Ok(None);
        }
        if box_px != self.reorder_box {
            // Frames were shrunk into the old box on the way in and cannot be
            // grown back; start collecting at the new size.
            self.reorder.clear();
            self.reorder_box = box_px;
        }
        let Some(entry) = self.container.index.entry(self.next_present_pos) else {
            return Ok(None);
        };
        let want_pts = entry.pts;

        let mut fed = 0usize;
        loop {
            if let Some(image) = self.reorder.remove(&want_pts) {
                self.next_present_pos += 1;
                let pts_secs = want_pts as f64 / self.container.index.timescale().max(1) as f64;
                return Ok(Some(VideoFrame { pts_secs, image }));
            }
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            if fed >= MAX_SAMPLES_PER_STEP {
                // A GOP longer than the budget, or a stream the decoder is not
                // emitting for. Give up on this exact frame rather than
                // blocking; the caller will ask again from the next position.
                self.next_present_pos += 1;
                return Ok(None);
            }
            if !self.feed_one(box_px)? {
                // Out of samples. Anything still parked is the tail of the
                // movie; hand back the latest of it so the panel does not go
                // blank at the end.
                let last = self.reorder.keys().next_back().copied();
                if let Some(pts) = last {
                    if let Some(image) = self.reorder.remove(&pts) {
                        self.next_present_pos = self.container.index.len();
                        let pts_secs = pts as f64 / self.container.index.timescale().max(1) as f64;
                        return Ok(Some(VideoFrame { pts_secs, image }));
                    }
                }
                return Ok(None);
            }
            fed += 1;
        }
    }
}
