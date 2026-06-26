/// M3U8 playlist generator for HLS
use std::collections::VecDeque;
use tracing::info;

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

    /// Add a new segment to the playlist
    pub fn add_segment(&mut self, duration: f64, filename: String) -> u64 {
        let seq = self.next_sequence;
        self.next_sequence += 1;

        let segment = Segment {
            sequence: seq,
            duration,
            filename,
        };

        self.segments.push_back(segment);

        // Remove old segments if we exceed max
        while self.segments.len() > self.max_segments {
            self.segments.pop_front();
            self.media_sequence += 1;
        }

        info!("[HLS] Added segment seq={}, duration={:.2}s, total segments={}", 
              seq, duration, self.segments.len());

        seq
    }

    /// Generate the M3U8 playlist content
    pub fn generate(&self) -> String {
        let mut output = String::new();

        output.push_str("#EXTM3U\r\n");
        output.push_str("#EXT-X-VERSION:3\r\n");
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

    /// Get the filename for a given sequence number
    pub fn get_segment_filename(sequence: u64) -> String {
        format!("segment_{}.ts", sequence)
    }
}
