/// MP4 Puller - pure Rust MP4 demux for remote VOD URLs.
/// Parses faststart AVC MP4 files and publishes H264 Annex-B frames.
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use bytes::Bytes;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use crate::core::{
    is_keyframe_annex_b, CodecType, MediaFrame, MediaTransport, StreamManager, StreamProtocol,
    StreamPuller, StreamSourceMode, TransportStatus, MILLISECOND_CLOCK_RATE,
};

const MAX_MOOV_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SAMPLE_BYTES: u32 = 16 * 1024 * 1024;

pub(crate) fn is_mp4_url(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.ends_with(".mp4") || path.ends_with(".m4v")
}

pub struct Mp4Puller {
    stream_manager: Arc<StreamManager>,
    remote_url: Option<String>,
    stream_id: String,
    peer_addr: String,
    status: TransportStatus,
}

impl Mp4Puller {
    pub fn new(stream_manager: Arc<StreamManager>) -> Self {
        Self {
            stream_manager,
            remote_url: None,
            stream_id: String::new(),
            peer_addr: String::new(),
            status: TransportStatus::Idle,
        }
    }

    pub fn with_url(mut self, remote_url: &str, stream_id: &str) -> Self {
        self.remote_url = Some(remote_url.to_string());
        self.stream_id = stream_id.to_string();
        self
    }

    async fn do_pull(&self, remote_url: &str, stream_id: &str) -> Result<()> {
        info!("[MP4 Puller] =========================================");
        info!(
            "[MP4 Puller] Starting MP4 pull from {} to stream {}",
            remote_url, stream_id
        );
        info!("[MP4 Puller] =========================================");

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;

        let session = self.stream_manager.begin_pull_loop(stream_id);
        self.stream_manager.create_stream(
            stream_id,
            StreamSourceMode::Pull,
            StreamProtocol::MP4,
            Some(remote_url.to_string()),
        );
        let _ = self.stream_manager.set_unpublished(stream_id);
        self.stream_manager.set_stream_broadcast(stream_id);
        let _ = self.stream_manager.set_publishing(stream_id);

        let movie = fetch_mp4_metadata(&client, remote_url).await?;
        info!(
            "[MP4 Puller] Parsed MP4 stream='{}' duration_ms={} timescale={} samples={} nal_len={} sps={} pps={}",
            stream_id,
            movie.duration_ms(),
            movie.video.timescale,
            movie.video.samples.len(),
            movie.video.nal_length_size,
            movie.video.sps.len(),
            movie.video.pps.len()
        );

        self.stream_manager
            .ensure_video_track_codec(stream_id, CodecType::H264);
        self.stream_manager.set_stream_sps_pps(
            stream_id,
            movie.video.sps.clone(),
            movie.video.pps.clone(),
        );

        let mut published = 0usize;
        let mut last_dts_ms = None;
        for sample in &movie.video.samples {
            if session.is_stopped() {
                info!(
                    "[MP4 Puller] Pull stopped for stream '{}', exiting sample loop",
                    stream_id
                );
                break;
            }

            if sample.size == 0 || sample.size > MAX_SAMPLE_BYTES {
                warn!(
                    "[MP4 Puller] Skip invalid sample stream='{}' offset={} size={}",
                    stream_id, sample.offset, sample.size
                );
                continue;
            }

            if let Some(prev_ms) = last_dts_ms {
                let delay = sample.dts_ms.saturating_sub(prev_ms).min(200);
                if delay > 0 {
                    sleep(Duration::from_millis(delay)).await;
                }
            }
            last_dts_ms = Some(sample.dts_ms);

            let data =
                fetch_range_exact(&client, remote_url, sample.offset, u64::from(sample.size))
                    .await
                    .with_context(|| {
                        format!(
                            "fetch MP4 sample offset={} size={}",
                            sample.offset, sample.size
                        )
                    })?;
            let annex_b = avc_sample_to_annex_b(
                &data,
                movie.video.nal_length_size,
                if sample.is_sync {
                    Some((&movie.video.sps, &movie.video.pps))
                } else {
                    None
                },
            )?;
            if annex_b.is_empty() {
                continue;
            }
            let is_keyframe = sample.is_sync || is_keyframe_annex_b(CodecType::H264, &annex_b);
            if is_keyframe && published == 0 {
                info!(
                    "[MP4 Puller] Published first keyframe for stream '{}'",
                    stream_id
                );
            }

            self.stream_manager.publish_frame(
                MediaFrame::new(
                    stream_id.to_string(),
                    0,
                    sample.dts_ms,
                    Bytes::from(annex_b),
                    is_keyframe,
                    CodecType::H264,
                )
                .with_clock_rate(MILLISECOND_CLOCK_RATE),
            );
            published += 1;
            if published <= 3 || published % 300 == 0 {
                debug!(
                    "[MP4 Puller] published video frame stream='{}' dts_ms={} size={} keyframe={} count={}",
                    stream_id, sample.dts_ms, sample.size, is_keyframe, published
                );
            }
        }

        info!(
            "[MP4 Puller] Finished MP4 pull stream='{}' published_video={}",
            stream_id, published
        );
        let _ = self.stream_manager.set_stopped(stream_id);
        self.stream_manager.end_pull_loop(stream_id, &session);
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct Mp4Movie {
    duration: u64,
    timescale: u32,
    video: Mp4VideoTrack,
}

impl Mp4Movie {
    fn duration_ms(&self) -> u64 {
        scale_time(self.duration, self.timescale, 1_000)
    }
}

#[derive(Debug, Clone)]
struct Mp4VideoTrack {
    timescale: u32,
    nal_length_size: usize,
    sps: Vec<u8>,
    pps: Vec<u8>,
    samples: Vec<Mp4Sample>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Mp4Sample {
    offset: u64,
    size: u32,
    dts_ms: u64,
    is_sync: bool,
}

#[derive(Debug, Default)]
struct TrackBuilder {
    is_video: bool,
    timescale: u32,
    sample_sizes: Vec<u32>,
    stts: Vec<SttsEntry>,
    stsc: Vec<StscEntry>,
    chunk_offsets: Vec<u64>,
    sync_samples: Vec<u32>,
    nal_length_size: usize,
    sps: Vec<u8>,
    pps: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
struct SttsEntry {
    count: u32,
    delta: u32,
}

#[derive(Debug, Clone, Copy)]
struct StscEntry {
    first_chunk: u32,
    samples_per_chunk: u32,
}

#[derive(Debug, Clone, Copy)]
struct BoxHeader {
    name: [u8; 4],
    header_size: usize,
    size: u64,
}

async fn fetch_mp4_metadata(client: &reqwest::Client, url: &str) -> Result<Mp4Movie> {
    let mut offset = 0u64;
    let mut scanned = 0u64;
    loop {
        if scanned > MAX_MOOV_BYTES {
            return Err(anyhow!(
                "moov box not found within first {} bytes",
                MAX_MOOV_BYTES
            ));
        }
        let head = fetch_range_exact(client, url, offset, 32).await?;
        let header = parse_box_header(&head, 0)?;
        let name = std::str::from_utf8(&header.name).unwrap_or("????");
        debug!(
            "[MP4 Puller] top-level box '{}' offset={} size={}",
            name, offset, header.size
        );
        if &header.name == b"moov" {
            if header.size > MAX_MOOV_BYTES {
                return Err(anyhow!("moov box too large: {} bytes", header.size));
            }
            let moov = fetch_range_exact(client, url, offset, header.size).await?;
            debug!(
                "[MP4 Puller] fetched moov bytes offset={} size={}",
                offset,
                moov.len()
            );
            return parse_moov(&moov);
        }
        if header.size == 0 {
            return Err(anyhow!("top-level box '{}' has unsupported size=0", name));
        }
        offset = offset.saturating_add(header.size);
        scanned = scanned.saturating_add(header.size);
    }
}

async fn fetch_range_exact(
    client: &reqwest::Client,
    url: &str,
    start: u64,
    len: u64,
) -> Result<Vec<u8>> {
    if len == 0 {
        return Ok(Vec::new());
    }
    let end = start + len - 1;
    let response = client
        .get(url)
        .header(reqwest::header::RANGE, format!("bytes={}-{}", start, end))
        .send()
        .await?;
    let status = response.status();
    if !(status == reqwest::StatusCode::PARTIAL_CONTENT || status.is_success()) {
        return Err(anyhow!("HTTP {} fetching range {}-{}", status, start, end));
    }
    let bytes = response.bytes().await?;
    if status != reqwest::StatusCode::PARTIAL_CONTENT && start > 0 {
        return Err(anyhow!(
            "server ignored Range request for {}-{}: status={} body_len={}",
            start,
            end,
            status,
            bytes.len()
        ));
    }
    if bytes.len() < len as usize {
        return Err(anyhow!(
            "short range body fetching {}-{}: got {} expected {}",
            start,
            end,
            bytes.len(),
            len
        ));
    }
    Ok(bytes[..len as usize].to_vec())
}

fn parse_moov(data: &[u8]) -> Result<Mp4Movie> {
    let root = parse_box_header(data, 0)?;
    if &root.name != b"moov" {
        return Err(anyhow!("expected moov box"));
    }
    debug!("[MP4 Puller] parsing moov bytes={}", data.len());

    let mut duration = 0u64;
    let mut timescale = 1u32;
    let mut tracks = Vec::new();
    for_each_child(
        data,
        root.header_size,
        root.size as usize,
        |name, payload| {
            match name {
                b"mvhd" => {
                    let (ts, dur) = parse_mvhd(payload)?;
                    timescale = ts;
                    duration = dur;
                }
                b"trak" => tracks.push(parse_trak(payload)?),
                _ => {}
            }
            Ok(())
        },
    )?;

    let video = tracks
        .into_iter()
        .find(|t| t.is_video)
        .ok_or_else(|| anyhow!("MP4 has no AVC video track"))?
        .build_video_track()?;
    debug!(
        "[MP4 Puller] parsed moov timescale={} duration={} video_samples={}",
        timescale,
        duration,
        video.samples.len()
    );
    Ok(Mp4Movie {
        duration,
        timescale,
        video,
    })
}

fn parse_trak(data: &[u8]) -> Result<TrackBuilder> {
    let mut track = TrackBuilder::default();
    for_each_box(data, |name, payload| {
        if name == b"mdia" {
            parse_mdia(payload, &mut track)?;
        }
        Ok(())
    })?;
    Ok(track)
}

fn parse_mdia(data: &[u8], track: &mut TrackBuilder) -> Result<()> {
    for_each_box(data, |name, payload| {
        match name {
            b"mdhd" => {
                let (ts, _) = parse_mdhd(payload)?;
                track.timescale = ts;
            }
            b"hdlr" => {
                if payload.len() >= 12 && &payload[8..12] == b"vide" {
                    track.is_video = true;
                }
            }
            b"minf" => parse_minf(payload, track)?,
            _ => {}
        }
        Ok(())
    })
}

fn parse_minf(data: &[u8], track: &mut TrackBuilder) -> Result<()> {
    for_each_box(data, |name, payload| {
        if name == b"stbl" {
            parse_stbl(payload, track)?;
        }
        Ok(())
    })
}

fn parse_stbl(data: &[u8], track: &mut TrackBuilder) -> Result<()> {
    for_each_box(data, |name, payload| {
        match name {
            b"stsd" => parse_stsd(payload, track)?,
            b"stts" => track.stts = parse_stts(payload)?,
            b"stsc" => track.stsc = parse_stsc(payload)?,
            b"stsz" => track.sample_sizes = parse_stsz(payload)?,
            b"stco" => track.chunk_offsets = parse_stco(payload)?,
            b"co64" => track.chunk_offsets = parse_co64(payload)?,
            b"stss" => track.sync_samples = parse_stss(payload)?,
            _ => {}
        }
        Ok(())
    })
}

fn parse_stsd(data: &[u8], track: &mut TrackBuilder) -> Result<()> {
    if data.len() < 8 {
        return Err(anyhow!("stsd too small"));
    }
    let entry_count = be_u32(data, 4)? as usize;
    let mut pos = 8usize;
    for _ in 0..entry_count {
        let header = parse_box_header(data, pos)?;
        let end = pos + header.size as usize;
        if &header.name == b"avc1" || &header.name == b"avc3" {
            track.is_video = true;
            let child_start = pos + header.header_size + 78;
            if child_start <= end {
                for_each_child(data, child_start, end, |name, payload| {
                    if name == b"avcC" {
                        parse_avcc(payload, track)?;
                    }
                    Ok(())
                })?;
            }
        }
        pos = end;
    }
    Ok(())
}

fn parse_avcc(data: &[u8], track: &mut TrackBuilder) -> Result<()> {
    if data.len() < 7 {
        return Err(anyhow!("avcC too small"));
    }
    track.nal_length_size = usize::from((data[4] & 0x03) + 1);
    let sps_count = data[5] & 0x1F;
    let mut pos = 6usize;
    for idx in 0..sps_count {
        let len = be_u16(data, pos)? as usize;
        pos += 2;
        if pos + len > data.len() {
            return Err(anyhow!("avcC SPS out of bounds"));
        }
        if idx == 0 {
            track.sps = data[pos..pos + len].to_vec();
        }
        pos += len;
    }
    if pos >= data.len() {
        return Err(anyhow!("avcC missing PPS count"));
    }
    let pps_count = data[pos];
    pos += 1;
    for idx in 0..pps_count {
        let len = be_u16(data, pos)? as usize;
        pos += 2;
        if pos + len > data.len() {
            return Err(anyhow!("avcC PPS out of bounds"));
        }
        if idx == 0 {
            track.pps = data[pos..pos + len].to_vec();
        }
        pos += len;
    }
    Ok(())
}

fn parse_mvhd(data: &[u8]) -> Result<(u32, u64)> {
    if data.is_empty() {
        return Err(anyhow!("mvhd too small"));
    }
    let version = data[0];
    if version == 1 {
        Ok((be_u32(data, 20)?, be_u64(data, 24)?))
    } else {
        Ok((be_u32(data, 12)?, u64::from(be_u32(data, 16)?)))
    }
}

fn parse_mdhd(data: &[u8]) -> Result<(u32, u64)> {
    if data.is_empty() {
        return Err(anyhow!("mdhd too small"));
    }
    let version = data[0];
    if version == 1 {
        Ok((be_u32(data, 20)?, be_u64(data, 24)?))
    } else {
        Ok((be_u32(data, 12)?, u64::from(be_u32(data, 16)?)))
    }
}

fn parse_stts(data: &[u8]) -> Result<Vec<SttsEntry>> {
    if data.len() < 8 {
        return Err(anyhow!("stts too small"));
    }
    let count = be_u32(data, 4)? as usize;
    let mut out = Vec::with_capacity(count);
    let mut pos = 8usize;
    for _ in 0..count {
        out.push(SttsEntry {
            count: be_u32(data, pos)?,
            delta: be_u32(data, pos + 4)?,
        });
        pos += 8;
    }
    Ok(out)
}

fn parse_stsc(data: &[u8]) -> Result<Vec<StscEntry>> {
    if data.len() < 8 {
        return Err(anyhow!("stsc too small"));
    }
    let count = be_u32(data, 4)? as usize;
    let mut out = Vec::with_capacity(count);
    let mut pos = 8usize;
    for _ in 0..count {
        out.push(StscEntry {
            first_chunk: be_u32(data, pos)?,
            samples_per_chunk: be_u32(data, pos + 4)?,
        });
        let _sample_description_index = be_u32(data, pos + 8)?;
        pos += 12;
    }
    Ok(out)
}

fn parse_stsz(data: &[u8]) -> Result<Vec<u32>> {
    if data.len() < 12 {
        return Err(anyhow!("stsz too small"));
    }
    let sample_size = be_u32(data, 4)?;
    let sample_count = be_u32(data, 8)? as usize;
    if sample_size != 0 {
        return Ok(vec![sample_size; sample_count]);
    }
    let mut out = Vec::with_capacity(sample_count);
    let mut pos = 12usize;
    for _ in 0..sample_count {
        out.push(be_u32(data, pos)?);
        pos += 4;
    }
    Ok(out)
}

fn parse_stco(data: &[u8]) -> Result<Vec<u64>> {
    if data.len() < 8 {
        return Err(anyhow!("stco too small"));
    }
    let count = be_u32(data, 4)? as usize;
    let mut out = Vec::with_capacity(count);
    let mut pos = 8usize;
    for _ in 0..count {
        out.push(u64::from(be_u32(data, pos)?));
        pos += 4;
    }
    Ok(out)
}

fn parse_co64(data: &[u8]) -> Result<Vec<u64>> {
    if data.len() < 8 {
        return Err(anyhow!("co64 too small"));
    }
    let count = be_u32(data, 4)? as usize;
    let mut out = Vec::with_capacity(count);
    let mut pos = 8usize;
    for _ in 0..count {
        out.push(be_u64(data, pos)?);
        pos += 8;
    }
    Ok(out)
}

fn parse_stss(data: &[u8]) -> Result<Vec<u32>> {
    if data.len() < 8 {
        return Err(anyhow!("stss too small"));
    }
    let count = be_u32(data, 4)? as usize;
    let mut out = Vec::with_capacity(count);
    let mut pos = 8usize;
    for _ in 0..count {
        out.push(be_u32(data, pos)?);
        pos += 4;
    }
    Ok(out)
}

impl TrackBuilder {
    fn build_video_track(self) -> Result<Mp4VideoTrack> {
        if self.timescale == 0 {
            return Err(anyhow!("video track has zero timescale"));
        }
        if self.nal_length_size == 0 || self.sps.is_empty() || self.pps.is_empty() {
            return Err(anyhow!("only AVC/H264 MP4 with avcC SPS/PPS is supported"));
        }
        let offsets = build_sample_offsets(&self)?;
        let dts = build_sample_dts_ms(&self.sample_sizes, &self.stts, self.timescale)?;
        let sync = self.sync_samples;
        let mut samples = Vec::with_capacity(self.sample_sizes.len());
        for (idx, size) in self.sample_sizes.iter().enumerate() {
            let number = (idx + 1) as u32;
            samples.push(Mp4Sample {
                offset: offsets[idx],
                size: *size,
                dts_ms: dts[idx],
                is_sync: sync.is_empty() || sync.binary_search(&number).is_ok(),
            });
        }
        Ok(Mp4VideoTrack {
            timescale: self.timescale,
            nal_length_size: self.nal_length_size,
            sps: self.sps,
            pps: self.pps,
            samples,
        })
    }
}

fn build_sample_offsets(track: &TrackBuilder) -> Result<Vec<u64>> {
    if track.stsc.is_empty() || track.chunk_offsets.is_empty() {
        return Err(anyhow!("missing stsc/stco sample offset tables"));
    }
    let mut out = Vec::with_capacity(track.sample_sizes.len());
    let mut sample_index = 0usize;
    let mut stsc_index = 0usize;
    for chunk_idx in 0..track.chunk_offsets.len() {
        let chunk_number = (chunk_idx + 1) as u32;
        while stsc_index + 1 < track.stsc.len()
            && track.stsc[stsc_index + 1].first_chunk <= chunk_number
        {
            stsc_index += 1;
        }
        if track.stsc[stsc_index].first_chunk > chunk_number {
            return Err(anyhow!("no stsc entry for chunk {}", chunk_number));
        }
        let samples_per_chunk = track.stsc[stsc_index].samples_per_chunk;
        let mut offset = track.chunk_offsets[chunk_idx];
        for _ in 0..samples_per_chunk {
            if sample_index >= track.sample_sizes.len() {
                break;
            }
            out.push(offset);
            offset = offset.saturating_add(u64::from(track.sample_sizes[sample_index]));
            sample_index += 1;
        }
    }
    if out.len() != track.sample_sizes.len() {
        return Err(anyhow!(
            "sample offset table mismatch: offsets={} samples={}",
            out.len(),
            track.sample_sizes.len()
        ));
    }
    Ok(out)
}

fn build_sample_dts_ms(sizes: &[u32], stts: &[SttsEntry], timescale: u32) -> Result<Vec<u64>> {
    let mut out = Vec::with_capacity(sizes.len());
    let mut dts = 0u64;
    for entry in stts {
        for _ in 0..entry.count {
            if out.len() >= sizes.len() {
                break;
            }
            out.push(scale_time(dts, timescale, 1_000));
            dts = dts.saturating_add(u64::from(entry.delta));
        }
    }
    if out.len() < sizes.len() {
        return Err(anyhow!(
            "stts sample count too small: dts={} samples={}",
            out.len(),
            sizes.len()
        ));
    }
    Ok(out)
}

fn avc_sample_to_annex_b(
    data: &[u8],
    nal_length_size: usize,
    config: Option<(&[u8], &[u8])>,
) -> Result<Vec<u8>> {
    if !(1..=4).contains(&nal_length_size) {
        return Err(anyhow!("invalid AVC nal length size {}", nal_length_size));
    }
    let mut out = Vec::with_capacity(data.len() + 64);
    if let Some((sps, pps)) = config {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(sps);
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(pps);
    }
    let mut pos = 0usize;
    while pos + nal_length_size <= data.len() {
        let mut len = 0usize;
        for b in &data[pos..pos + nal_length_size] {
            len = (len << 8) | usize::from(*b);
        }
        pos += nal_length_size;
        if len == 0 {
            continue;
        }
        if pos + len > data.len() {
            return Err(anyhow!(
                "AVC sample NAL out of bounds: pos={} len={} sample={}",
                pos,
                len,
                data.len()
            ));
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&data[pos..pos + len]);
        pos += len;
    }
    Ok(out)
}

fn parse_box_header(data: &[u8], pos: usize) -> Result<BoxHeader> {
    if pos + 8 > data.len() {
        return Err(anyhow!("box header out of bounds"));
    }
    let size32 = be_u32(data, pos)?;
    let name = [data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]];
    if size32 == 1 {
        let size = be_u64(data, pos + 8)?;
        Ok(BoxHeader {
            name,
            header_size: 16,
            size,
        })
    } else {
        Ok(BoxHeader {
            name,
            header_size: 8,
            size: u64::from(size32),
        })
    }
}

fn for_each_box<F>(data: &[u8], mut f: F) -> Result<()>
where
    F: FnMut(&[u8; 4], &[u8]) -> Result<()>,
{
    for_each_child(data, 0, data.len(), |name, payload| f(name, payload))
}

fn for_each_child<F>(data: &[u8], mut pos: usize, end: usize, mut f: F) -> Result<()>
where
    F: FnMut(&[u8; 4], &[u8]) -> Result<()>,
{
    while pos + 8 <= end && pos + 8 <= data.len() {
        let header = parse_box_header(data, pos)?;
        if header.size < header.header_size as u64 {
            return Err(anyhow!("invalid MP4 box size"));
        }
        let box_end = pos
            .checked_add(header.size as usize)
            .ok_or_else(|| anyhow!("MP4 box offset overflow"))?;
        if box_end > end || box_end > data.len() {
            return Err(anyhow!("MP4 box out of bounds"));
        }
        let payload_start = pos + header.header_size;
        f(&header.name, &data[payload_start..box_end])?;
        pos = box_end;
    }
    Ok(())
}

fn scale_time(value: u64, from_timescale: u32, to_timescale: u64) -> u64 {
    if from_timescale == 0 {
        return 0;
    }
    value.saturating_mul(to_timescale) / u64::from(from_timescale)
}

fn be_u16(data: &[u8], pos: usize) -> Result<u16> {
    if pos + 2 > data.len() {
        return Err(anyhow!("u16 out of bounds"));
    }
    Ok(u16::from_be_bytes([data[pos], data[pos + 1]]))
}

fn be_u32(data: &[u8], pos: usize) -> Result<u32> {
    if pos + 4 > data.len() {
        return Err(anyhow!("u32 out of bounds"));
    }
    Ok(u32::from_be_bytes([
        data[pos],
        data[pos + 1],
        data[pos + 2],
        data[pos + 3],
    ]))
}

fn be_u64(data: &[u8], pos: usize) -> Result<u64> {
    if pos + 8 > data.len() {
        return Err(anyhow!("u64 out of bounds"));
    }
    Ok(u64::from_be_bytes([
        data[pos],
        data[pos + 1],
        data[pos + 2],
        data[pos + 3],
        data[pos + 4],
        data[pos + 5],
        data[pos + 6],
        data[pos + 7],
    ]))
}

#[async_trait]
impl MediaTransport for Mp4Puller {
    fn protocol(&self) -> StreamProtocol {
        StreamProtocol::MP4
    }

    fn status(&self) -> TransportStatus {
        self.status.clone()
    }

    fn peer_addr(&self) -> String {
        self.peer_addr.clone()
    }

    fn stream_id(&self) -> String {
        self.stream_id.clone()
    }

    async fn start(&mut self) -> Result<()> {
        self.status = TransportStatus::Connecting;
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        self.status = TransportStatus::Closed;
        Ok(())
    }
}

#[async_trait]
impl StreamPuller for Mp4Puller {
    async fn pull(&mut self) -> Result<()> {
        let Some(remote_url) = self.remote_url.clone() else {
            return Err(anyhow!("remote URL not set"));
        };
        if self.stream_id.is_empty() {
            return Err(anyhow!("stream ID not set"));
        }
        self.status = TransportStatus::Publishing;
        match self.do_pull(&remote_url, &self.stream_id).await {
            Ok(()) => {
                self.status = TransportStatus::Closed;
                Ok(())
            }
            Err(e) => {
                error!("[MP4 Puller] Pull failed: {}", e);
                let _ = self
                    .stream_manager
                    .set_error(&self.stream_id, &e.to_string());
                self.status = TransportStatus::Error(e.to_string());
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avc_sample_to_annex_b_prepends_config_on_keyframe() {
        let sample = [0, 0, 0, 2, 0x65, 0x88];
        let out = avc_sample_to_annex_b(&sample, 4, Some((&[0x67], &[0x68]))).unwrap();

        assert_eq!(
            out,
            vec![0, 0, 0, 1, 0x67, 0, 0, 0, 1, 0x68, 0, 0, 0, 1, 0x65, 0x88]
        );
    }

    #[test]
    fn sample_offsets_follow_stsc_chunks() {
        let track = TrackBuilder {
            sample_sizes: vec![10, 20, 30],
            stsc: vec![StscEntry {
                first_chunk: 1,
                samples_per_chunk: 2,
            }],
            chunk_offsets: vec![100, 200],
            ..Default::default()
        };

        assert_eq!(build_sample_offsets(&track).unwrap(), vec![100, 110, 200]);
    }

    #[test]
    fn parse_downloaded_faststart_moov_when_available() {
        let Ok(data) = std::fs::read("/tmp/vcp_mp4_moov.bin") else {
            return;
        };

        let movie = parse_moov(&data).unwrap();
        assert_eq!(movie.video.nal_length_size, 4);
        assert!(!movie.video.sps.is_empty());
        assert!(!movie.video.pps.is_empty());
        assert!(!movie.video.samples.is_empty());
    }
}
