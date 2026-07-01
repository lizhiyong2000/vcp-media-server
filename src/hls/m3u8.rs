/// M3U8 playlist generator for HLS
use std::collections::VecDeque;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    /// Media start time: live_edge - duration (pairs with segment-local TS PTS)
    pub program_date_time: SystemTime,
    /// Whether this segment starts after a timestamp discontinuity
    pub discontinuity: bool,
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
    /// Count of media discontinuities before the first segment in the playlist
    discontinuity_sequence: u64,
    /// Whether the stream has ended
    ended: bool,
}

impl M3u8Generator {
    pub fn new(target_duration: f64, max_segments: usize) -> Self {
        Self {
            target_duration,
            max_segments: max_segments.max(1),
            media_sequence: 0,
            segments: VecDeque::new(),
            next_sequence: 0,
            discontinuity_sequence: 0,
            ended: false,
        }
    }

    /// Get the target segment duration
    pub fn target_duration(&self) -> f64 {
        self.target_duration
    }

    /// Add a committed segment. `program_date_time` must match the session mux timeline
    /// (chained across segments); set `discontinuity` only after a timestamp reset (lag snap).
    pub fn add_segment(
        &mut self,
        duration: f64,
        sequence: u64,
        program_date_time: SystemTime,
        discontinuity: bool,
    ) {
        let filename = Self::segment_filename(sequence);

        let segment = Segment {
            sequence,
            duration,
            filename,
            program_date_time,
            discontinuity,
        };

        self.segments.push_back(segment);
        self.next_sequence = sequence + 1;

        while self.segments.len() > self.max_segments {
            if let Some(removed) = self.segments.pop_front() {
                self.media_sequence += 1;
                if removed.discontinuity {
                    self.discontinuity_sequence += 1;
                }
            }
        }

        debug!(
            "[HLS] Added segment seq={}, duration={:.2}s, media_seq={}, window={}",
            sequence,
            duration,
            self.media_sequence,
            self.segments.len()
        );
    }

    fn playlist_target_duration(&self) -> u64 {
        let observed = self
            .segments
            .iter()
            .map(|s| s.duration)
            .fold(self.target_duration, f64::max);
        observed.ceil().max(1.0) as u64
    }

    fn format_program_date_time(t: SystemTime) -> String {
        let dur = t.duration_since(UNIX_EPOCH).unwrap_or_default();
        let total_ms = dur.as_millis();
        let secs = (total_ms / 1000) as i64;
        let ms = (total_ms % 1000) as u32;
        time::OffsetDateTime::from_unix_timestamp(secs)
            .map(|dt| {
                format!(
                    "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
                    dt.year(),
                    dt.month() as u8,
                    dt.day(),
                    dt.hour(),
                    dt.minute(),
                    dt.second(),
                    ms
                )
            })
            .unwrap_or_else(|_| "1970-01-01T00:00:00.000Z".to_string())
    }

    /// Generate the M3U8 playlist content (live sliding window).
    pub fn generate(&self) -> String {
        let mut output = String::new();

        output.push_str("#EXTM3U\r\n");
        output.push_str("#EXT-X-VERSION:3\r\n");
        output.push_str("#EXT-X-INDEPENDENT-SEGMENTS\r\n");
        output.push_str(&format!(
            "#EXT-X-TARGETDURATION:{}\r\n",
            self.playlist_target_duration()
        ));
        output.push_str(&format!(
            "#EXT-X-MEDIA-SEQUENCE:{}\r\n",
            self.media_sequence
        ));
        if self.discontinuity_sequence > 0 {
            output.push_str(&format!(
                "#EXT-X-DISCONTINUITY-SEQUENCE:{}\r\n",
                self.discontinuity_sequence
            ));
        }

        for segment in &self.segments {
            if segment.discontinuity {
                output.push_str("#EXT-X-DISCONTINUITY\r\n");
            }
            output.push_str(&format!(
                "#EXT-X-PROGRAM-DATE-TIME:{}\r\n",
                Self::format_program_date_time(segment.program_date_time)
            ));
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

    /// Unique segment filename (avoids HTTP cache collisions on live rewrite).
    pub fn segment_filename(sequence: u64) -> String {
        format!("segment_{sequence}.ts")
    }

    /// Slot-based segment filename (legacy).
    pub fn slot_filename(sequence: u64, max_segments: usize) -> String {
        Self::segment_filename(sequence)
    }

    pub fn max_segments(&self) -> usize {
        self.max_segments
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    #[test]
    fn target_duration_stays_at_one_for_sub_second_extinf() {
        let mut gen = M3u8Generator::new(1.0, 3);
        gen.add_segment(0.96, 0, SystemTime::now(), false);
        let pl = gen.generate();
        assert!(pl.contains("#EXT-X-TARGETDURATION:1"));
        assert!(pl.contains("#EXTINF:0.960,"));
        assert!(!pl.contains("TARGETDURATION:3"));
    }

    #[test]
    fn playlist_carries_program_date_time_per_segment() {
        let mut gen = M3u8Generator::new(1.0, 1);
        let pdt = SystemTime::now();
        gen.add_segment(1.0, 0, pdt, false);
        let pl = gen.generate();
        assert!(pl.contains("#EXT-X-PROGRAM-DATE-TIME:"));
        assert!(pl.contains("#EXTINF:1.000,"));
    }

    #[test]
    fn discontinuity_sequence_counts_only_segments_before_window() {
        let mut gen = M3u8Generator::new(1.0, 2);
        let now = SystemTime::now();

        gen.add_segment(1.0, 0, now, false);
        gen.add_segment(1.0, 1, now, true);
        let pl = gen.generate();
        assert!(!pl.contains("#EXT-X-DISCONTINUITY-SEQUENCE:"));
        assert!(pl.contains("#EXT-X-DISCONTINUITY\r\n"));

        gen.add_segment(1.0, 2, now, false);
        let pl = gen.generate();
        assert!(!pl.contains("#EXT-X-DISCONTINUITY-SEQUENCE:"));

        gen.add_segment(1.0, 3, now, false);
        let pl = gen.generate();
        assert!(pl.contains("#EXT-X-DISCONTINUITY-SEQUENCE:1"));
        assert!(!pl.contains("#EXT-X-DISCONTINUITY\r\n"));
    }
}
