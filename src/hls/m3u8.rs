/// M3U8 playlist generator for HLS
use std::collections::VecDeque;
use tracing::debug;

/// A single segment in the playlist
#[derive(Debug, Clone)]
pub struct Segment {
    /// Segment sequence number
    pub sequence: u64,
    /// Duration in seconds
    pub duration: f64,
    /// File name (e.g., "segment_0.ts")
    pub filename: String,
}

/// M3U8 playlist generator
pub struct M3u8Generator {
    /// Target segment duration in seconds
    pub target_duration: f64,
    /// Maximum number of segments to keep in the playlist
    pub max_segments: usize,
    /// Media sequence number (increments as segments are removed)
    media_sequence: u64,
    /// List of segments
    segments: VecDeque<Segment>,
    /// Next segment sequence number
    next_sequence: u64,
    /// Whether the stream has ended
    ended: bool,
}

impl M3u8Generator {
    pub fn new(target_duration: f64, max_segments: usize) -> Self {
        Self {
            target_duration,
            max_segments,
            media_sequence: 0,
            segments: VecDeque::new(),
            next_sequence: 0,
            ended: false,
        }
    }

    /// Get the target segment duration
    pub fn target_duration(&self) -> f64 {
        self.target_duration
    }

    /// Add a committed segment to the playlist (slot-based filename, overwrites on disk).
    pub fn add_segment(&mut self, duration: f64, sequence: u64) {
        let filename = Self::slot_filename(sequence, self.max_segments);

        let segment = Segment {
            sequence,
            duration,
            filename,
        };

        self.segments.push_back(segment);
        self.next_sequence = sequence + 1;

        while self.segments.len() > self.max_segments {
            if self.segments.pop_front().is_some() {
                self.media_sequence += 1;
            }
        }

        debug!(
            "[HLS] Added segment seq={}, slot={}, duration={:.2}s, media_seq={}, window={}",
            sequence,
            sequence % self.max_segments as u64,
            duration,
            self.media_sequence,
            self.segments.len()
        );
    }

    /// Generate the M3U8 playlist content
    pub fn generate(&self) -> String {
        let mut output = String::new();

        output.push_str("#EXTM3U\r\n");
        output.push_str("#EXT-X-VERSION:3\r\n");
        output.push_str("#EXT-X-INDEPENDENT-SEGMENTS\r\n");
        output.push_str(&format!("#EXT-X-TARGETDURATION:{}\r\n", self.target_duration.ceil() as u64));
        output.push_str(&format!("#EXT-X-MEDIA-SEQUENCE:{}\r\n", self.media_sequence));

        for segment in &self.segments {
            output.push_str(&format!("#EXTINF:{:.3},\r\n", segment.duration));
            output.push_str(&segment.filename);
            output.push_str("\r\n");
        }

        if self.ended {
            output.push_str("#EXT-X-ENDLIST\r\n");
        }

        output
    }

    /// Get the current media sequence number
    pub fn media_sequence(&self) -> u64 {
        self.media_sequence
    }

    /// Get the number of segments
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// Mark the stream as ended
    pub fn set_ended(&mut self) {
        self.ended = true;
    }

    /// Get the next expected sequence number
    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    /// Slot-based segment filename (fixed pool, overwritten each lap).
    pub fn slot_filename(sequence: u64, max_segments: usize) -> String {
        let slots = max_segments.max(1) as u64;
        format!("segment_{}.ts", sequence % slots)
    }

    pub fn max_segments(&self) -> usize {
        self.max_segments
    }
}
