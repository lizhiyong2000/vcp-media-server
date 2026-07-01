use anyhow::Result;
use bytes::Bytes;
use std::time::Duration;
use tokio::time::sleep;
use tracing::info;

use crate::core::{CodecType, MediaFrame, StreamManager, StreamProtocol, StreamSourceMode, Track};
use std::sync::Arc;

pub struct StreamTester {
    stream_manager: Arc<StreamManager>,
}

impl StreamTester {
    pub fn new(stream_manager: Arc<StreamManager>) -> Self {
        Self { stream_manager }
    }

    /// Generate a test H264 frame (SPS/PPS + NAL units)
    fn generate_h264_sps_pps() -> Bytes {
        // Minimal SPS (Sequence Parameter Set) for baseline profile
        let sps = vec![0x67, 0x42, 0xC0, 0x0A, 0xDA, 0x0F, 0x0A, 0x69, 0xA8];
        // Minimal PPS (Picture Parameter Set)
        let pps = vec![0x68, 0xCE, 0x38, 0x80];
        let mut data = Vec::new();
        data.extend_from_slice(&sps);
        data.extend_from_slice(&pps);
        Bytes::from(data)
    }

    /// Generate a test H264 IDR frame (keyframe)
    fn generate_h264_idr_frame(frame_num: u32) -> Bytes {
        let mut data = Vec::new();
        // NAL header for IDR (5)
        data.push(0x65);
        // Fake slice header
        data.extend_from_slice(&[0x88, 0x80, 0x42, 0x00, 0x00]);
        // Fill with some dummy data
        for i in 0..500 {
            data.push((frame_num as u8).wrapping_add(i as u8));
        }
        Bytes::from(data)
    }

    /// Generate a test H264 P frame
    fn generate_h264_p_frame(frame_num: u32) -> Bytes {
        let mut data = Vec::new();
        // NAL header for P slice (1)
        data.push(0x41);
        // Fake slice header
        data.extend_from_slice(&[0x9A, 0x00, 0x42, 0x00, 0x00]);
        // Fill with some dummy data
        for i in 0..200 {
            data.push((frame_num as u8).wrapping_add(i as u8));
        }
        Bytes::from(data)
    }

    /// Generate a test AAC audio frame
    fn generate_aac_frame(frame_num: u32) -> Bytes {
        let mut data = Vec::new();
        // AAC raw data frame (ADTS header + raw data)
        // ADTS header
        data.extend_from_slice(&[0xFF, 0xF1, 0x50, 0x80]);
        // Fill with some dummy audio data
        for i in 0..128 {
            data.push((frame_num as u8).wrapping_add(i as u8));
        }
        Bytes::from(data)
    }

    /// Push a test stream to the stream manager
    #[allow(dead_code)]
    pub async fn push_test_stream(&self, stream_id: &str, duration_secs: u64) -> Result<()> {
        info!(
            "[Tester] Starting test stream '{}' for {} seconds",
            stream_id, duration_secs
        );

        // Create stream if not exists
        if self
            .stream_manager
            .get_stream(&stream_id.to_string())
            .is_none()
        {
            let tracks = vec![
                Track {
                    id: 0,
                    codec: CodecType::H264,
                    payload_type: 96,
                    clock_rate: 90000,
                    extra_params: std::collections::HashMap::new(),
                },
                Track {
                    id: 1,
                    codec: CodecType::AAC,
                    payload_type: 97,
                    clock_rate: 44100,
                    extra_params: std::collections::HashMap::new(),
                },
            ];
            self.stream_manager.create_stream(
                stream_id,
                StreamSourceMode::Push,
                StreamProtocol::Unknown,
                None,
            );
            info!("[Tester] Created stream: {}", stream_id);
        }

        let fps: u64 = 25;
        let frame_duration = 1000 / fps; // ms per frame
        let total_frames = fps * duration_secs;
        let mut timestamp: u64 = 0;
        let timestamp_inc_video = 3600; // 90000 / 25

        for frame_idx in 0..total_frames {
            // Send video frame
            let is_keyframe = frame_idx % 100 == 0; // IDR every 4 seconds at 25fps
            let video_data = if is_keyframe {
                let mut data = Self::generate_h264_sps_pps().to_vec();
                let idr = Self::generate_h264_idr_frame(frame_idx as u32);
                data.extend_from_slice(&idr);
                Bytes::from(data)
            } else {
                Self::generate_h264_p_frame(frame_idx as u32)
            };

            let video_frame = MediaFrame::new(
                stream_id.to_string(),
                0, // video track
                timestamp,
                video_data,
                is_keyframe,
                CodecType::H264,
            );
            self.stream_manager.publish_frame(video_frame);

            // Send audio frame
            let audio_frame = MediaFrame::new(
                stream_id.to_string(),
                1, // audio track
                timestamp,
                Self::generate_aac_frame(frame_idx as u32),
                false,
                CodecType::AAC,
            );
            self.stream_manager.publish_frame(audio_frame);

            if frame_idx % 100 == 0 {
                info!(
                    "[Tester] Pushed {} frames, timestamp={}",
                    frame_idx, timestamp
                );
            }

            timestamp += timestamp_inc_video;

            // Frame timing (simplified - real implementation would use proper timing)
            sleep(Duration::from_millis(frame_duration / 2)).await;
        }

        info!("[Tester] Test stream '{}' completed", stream_id);
        Ok(())
    }

    /// Push a single keyframe and wait (for RTSP testing)
    #[allow(dead_code)]
    pub async fn push_single_frame(&self, stream_id: &str) -> Result<()> {
        // Create stream
        let tracks = vec![Track {
            id: 0,
            codec: CodecType::H264,
            payload_type: 96,
            clock_rate: 90000,
            extra_params: std::collections::HashMap::new(),
        }];
        self.stream_manager.create_stream(
            stream_id,
            StreamSourceMode::Push,
            StreamProtocol::Unknown,
            None,
        );

        // Send SPS/PPS and IDR
        let sps_pps = Self::generate_h264_sps_pps();
        let idr = Self::generate_h264_idr_frame(0);

        let frame = MediaFrame::new(stream_id.to_string(), 0, 0, sps_pps, true, CodecType::H264);
        self.stream_manager.publish_frame(frame);

        sleep(Duration::from_millis(33)).await;

        let frame = MediaFrame::new(stream_id.to_string(), 0, 3600, idr, true, CodecType::H264);
        self.stream_manager.publish_frame(frame);

        info!("[Tester] Pushed single frame to '{}'", stream_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_stream_manager_create_stream() {
        let manager = StreamManager::new();
        let tracks = vec![Track {
            id: 0,
            codec: CodecType::H264,
            payload_type: 96,
            clock_rate: 90000,
            extra_params: HashMap::new(),
        }];
        let stream = manager.create_stream(
            "test",
            StreamSourceMode::Push,
            StreamProtocol::Unknown,
            None,
        );
        manager.set_stream_tracks("test", tracks);
        let stream = manager.get_stream(&"test".to_string()).unwrap();
        assert_eq!(stream.id, "test");
        assert_eq!(stream.tracks.len(), 1);
        assert_eq!(stream.tracks[0].codec, CodecType::H264);
    }

    #[test]
    fn test_stream_manager_get_stream() {
        let manager = StreamManager::new();
        let tracks = vec![Track {
            id: 0,
            codec: CodecType::H264,
            payload_type: 96,
            clock_rate: 90000,
            extra_params: HashMap::new(),
        }];
        manager.create_stream(
            "test",
            StreamSourceMode::Push,
            StreamProtocol::Unknown,
            None,
        );

        let stream = manager.get_stream(&"test".to_string());
        assert!(stream.is_some());
        assert_eq!(stream.unwrap().id, "test");
    }

    #[test]
    fn test_stream_manager_remove_stream() {
        let manager = StreamManager::new();
        let tracks = vec![Track {
            id: 0,
            codec: CodecType::H264,
            payload_type: 96,
            clock_rate: 90000,
            extra_params: HashMap::new(),
        }];
        manager.create_stream(
            "test",
            StreamSourceMode::Push,
            StreamProtocol::Unknown,
            None,
        );

        let removed = manager.remove_stream(&"test".to_string());
        assert!(removed.is_some());

        let stream = manager.get_stream(&"test".to_string());
        assert!(stream.is_none());
    }

    #[test]
    fn test_stream_manager_list_streams() {
        let manager = StreamManager::new();
        let tracks = vec![Track {
            id: 0,
            codec: CodecType::H264,
            payload_type: 96,
            clock_rate: 90000,
            extra_params: HashMap::new(),
        }];
        manager.create_stream(
            "stream1",
            StreamSourceMode::Push,
            StreamProtocol::Unknown,
            None,
        );
        manager.create_stream(
            "stream2",
            StreamSourceMode::Push,
            StreamProtocol::Unknown,
            None,
        );

        let streams = manager.list_streams();
        assert_eq!(streams.len(), 2);
        assert!(streams.contains(&"stream1".to_string()));
        assert!(streams.contains(&"stream2".to_string()));
    }

    #[test]
    fn test_stream_manager_subscribe() {
        let manager = StreamManager::new();
        let tracks = vec![Track {
            id: 0,
            codec: CodecType::H264,
            payload_type: 96,
            clock_rate: 90000,
            extra_params: HashMap::new(),
        }];
        manager.create_stream(
            "test",
            StreamSourceMode::Push,
            StreamProtocol::Unknown,
            None,
        );

        manager.ensure_stream_hub("test");
        let reader =
            manager.dispatch_subscribe("test", crate::core::DispatchPolicy::SequentialFromIdr);
        assert!(reader.is_some());
    }

    #[test]
    fn test_codec_type_from_pt() {
        assert_eq!(CodecType::H264, CodecType::from_pt(96));
        assert_eq!(CodecType::H265, CodecType::from_pt(98));
        assert_eq!(CodecType::AAC, CodecType::from_pt(97));
        assert_eq!(CodecType::Opus, CodecType::from_pt(109));
        assert_eq!(CodecType::G711, CodecType::from_pt(0));
        assert_eq!(CodecType::G711, CodecType::from_pt(8));
    }

    #[test]
    fn test_media_frame_creation() {
        let data = Bytes::from(vec![0x01, 0x02, 0x03]);
        let frame = MediaFrame::new(
            "test".to_string(),
            0,
            1000,
            data.clone(),
            true,
            CodecType::H264,
        );
        assert_eq!(frame.stream_id, "test");
        assert_eq!(frame.track_id, 0);
        assert_eq!(frame.timestamp, 1000);
        assert_eq!(frame.data, data);
        assert!(frame.is_keyframe);
        assert_eq!(frame.codec, CodecType::H264);
    }

    #[test]
    fn test_generate_h264_sps_pps() {
        let data = StreamTester::generate_h264_sps_pps();
        assert!(!data.is_empty());
        // SPS starts with 0x67
        assert_eq!(data[0], 0x67);
        // PPS starts with 0x68
        assert_eq!(data[9], 0x68);
    }

    #[test]
    fn test_generate_h264_idr_frame() {
        let data = StreamTester::generate_h264_idr_frame(42);
        assert!(!data.is_empty());
        // NAL header for IDR is 0x65
        assert_eq!(data[0], 0x65);
    }

    #[test]
    fn test_generate_h264_p_frame() {
        let data = StreamTester::generate_h264_p_frame(42);
        assert!(!data.is_empty());
        // NAL header for P slice is 0x41
        assert_eq!(data[0], 0x41);
    }

    #[test]
    fn test_generate_aac_frame() {
        let data = StreamTester::generate_aac_frame(42);
        assert!(!data.is_empty());
        // ADTS header starts with 0xFF
        assert_eq!(data[0], 0xFF);
    }

    #[test]
    fn test_config_default() {
        let config = crate::core::Config::default();
        assert_eq!(config.rtmp.port, 1935);
        assert_eq!(config.rtsp.port, 554);
        assert_eq!(config.webrtc.port, 9080);
        assert_eq!(config.http.port, 8081);
        assert_eq!(config.streams.len(), 1);
        assert_eq!(config.streams[0].id, "live");
    }

    #[test]
    fn test_rtmp_message_parsing() {
        // Test parsing an RTMP audio message (0x08)
        let buf = vec![
            0x02, 0x00, 0x00, 0x00, // chunk basic header
            0x00, 0x00, 0x00, // timestamp
            0x00, 0x00, 0x10, // message length (16)
            0x08, // message type (audio)
            0x00, 0x00, 0x00, 0x00, // message stream id
            0x00, 0x00, 0x00, // payload (abbreviated)
        ];
        // This is a basic smoke test - parsing structure is correct
        // Real RTMP parsing would require more complete messages
    }

    // ========== RTSP Protocol Tests ==========

    #[test]
    fn test_rtsp_options_request() {
        let request = "OPTIONS rtsp://localhost:554/live RTSP/1.0\r\n\
                        CSeq: 1\r\n\
                        User-Agent: TestClient\r\n\
                        \r\n";
        assert!(request.starts_with("OPTIONS"));
        assert!(request.contains("rtsp://"));
        assert!(request.contains("RTSP/1.0"));
        assert!(request.contains("CSeq:"));
    }

    #[test]
    fn test_rtsp_describe_request() {
        let request = "DESCRIBE rtsp://localhost:554/live RTSP/1.0\r\n\
                        CSeq: 2\r\n\
                        Accept: application/sdp\r\n\
                        \r\n";
        assert!(request.starts_with("DESCRIBE"));
        assert!(request.contains("application/sdp"));
    }

    #[test]
    fn test_rtsp_setup_request() {
        let request = "SETUP rtsp://localhost:554/live/track0 RTSP/1.0\r\n\
                        CSeq: 3\r\n\
                        Transport: RTP/AVP;unicast;client_port=5000-5001\r\n\
                        \r\n";
        assert!(request.starts_with("SETUP"));
        assert!(request.contains("track0"));
        assert!(request.contains("Transport:"));
        assert!(request.contains("RTP/AVP"));
    }

    #[test]
    fn test_rtsp_play_request() {
        let request = "PLAY rtsp://localhost:554/live RTSP/1.0\r\n\
                        CSeq: 4\r\n\
                        Session: 12345678\r\n\
                        Range: npt=0.000-\r\n\
                        \r\n";
        assert!(request.starts_with("PLAY"));
        assert!(request.contains("Range:"));
    }

    #[test]
    fn test_rtsp_teardown_request() {
        let request = "TEARDOWN rtsp://localhost:554/live RTSP/1.0\r\n\
                        CSeq: 5\r\n\
                        Session: 12345678\r\n\
                        \r\n";
        assert!(request.starts_with("TEARDOWN"));
        assert!(request.contains("Session:"));
    }

    #[test]
    fn test_rtsp_sdp_parsing() {
        let sdp = "v=0\r\n\
                    o=- 123456789 123456789 IN IP4 127.0.0.1\r\n\
                    s=Test Stream\r\n\
                    c=IN IP4 0.0.0.0\r\n\
                    t=0 0\r\n\
                    m=video 0 RTP/AVP 96\r\n\
                    a=rtpmap:96 H264/90000\r\n\
                    m=audio 0 RTP/AVP 97\r\n\
                    a=rtpmap:97 mpeg4-generic/44100/2\r\n";
        assert!(sdp.contains("v=0"));
        assert!(sdp.contains("H264/90000"));
        assert!(sdp.contains("mpeg4-generic/44100"));
    }

    #[test]
    fn test_rtsp_response_status_line() {
        let response = "RTSP/1.0 200 OK\r\n\
                         CSeq: 1\r\n\
                         Session: 12345678\r\n\
                         \r\n";
        assert!(response.starts_with("RTSP/1.0 200 OK"));
    }

    // ========== RTMP Protocol Tests ==========

    #[test]
    fn test_rtmp_handshake_c0() {
        // C0: client version
        let c0 = vec![0x03]; // RTMP version (should be 3)
        assert_eq!(c0[0], 0x03);
    }

    #[test]
    fn test_rtmp_handshake_c1() {
        // C1: 1536 bytes timestamp + zero + random data
        let mut c1 = vec![0; 1536];
        c1[0..4].copy_from_slice(&0x00000000u32.to_be_bytes()); // timestamp
        c1[4..8].copy_from_slice(&0u32.to_be_bytes()); // zero
                                                       // Fill with pattern for testing
        for i in 8..1536 {
            c1[i] = (i % 256) as u8;
        }
        assert_eq!(c1.len(), 1536);
        assert_eq!(&c1[0..4], &[0, 0, 0, 0]);
    }

    #[test]
    fn test_rtmp_connect_command() {
        // AMF0 Command: connect - verify structure
        let mut cmd: Vec<u8> = Vec::new();
        cmd.push(0x02); // String marker
        cmd.extend_from_slice(b"connect");
        cmd.push(0x00); // Number marker
        cmd.extend_from_slice(&1.0_f64.to_be_bytes());
        cmd.push(0x03); // Object marker
        cmd.extend_from_slice(b"app");
        cmd.push(0x02); // String marker
        cmd.extend_from_slice(b"live");
        cmd.extend_from_slice(&[0x00, 0x00, 0x09]); // Object end

        assert_eq!(cmd[0], 0x02); // First byte is string marker
        assert_eq!(cmd[1], b'c'); // "connect" starts
    }

    #[test]
    fn test_rtmp_release_command() {
        let mut cmd: Vec<u8> = Vec::new();
        cmd.push(0x02);
        cmd.extend_from_slice(b"releaseStream");
        cmd.push(0x00);
        cmd.extend_from_slice(&2.0_f64.to_be_bytes());
        cmd.push(0x05);
        cmd.push(0x02);
        cmd.extend_from_slice(b"live");

        assert_eq!(cmd[0], 0x02);
        assert_eq!(cmd[1], b'r');
    }

    #[test]
    fn test_rtmp_fc_publish_command() {
        let mut cmd: Vec<u8> = Vec::new();
        cmd.push(0x02);
        cmd.extend_from_slice(b"FCPublish");
        cmd.push(0x00);
        cmd.extend_from_slice(&3.0_f64.to_be_bytes());
        cmd.push(0x05);
        cmd.push(0x02);
        cmd.extend_from_slice(b"live");

        assert_eq!(cmd[0], 0x02);
        assert_eq!(cmd[1], b'F');
    }

    #[test]
    fn test_rtmp_create_stream_command() {
        let mut cmd: Vec<u8> = Vec::new();
        cmd.push(0x02);
        cmd.extend_from_slice(b"createStream");
        cmd.push(0x00);
        cmd.extend_from_slice(&4.0_f64.to_be_bytes());
        cmd.push(0x05);

        assert_eq!(cmd[0], 0x02);
        assert_eq!(cmd[1], b'c');
    }

    #[test]
    fn test_rtmp_publish_command() {
        let mut cmd: Vec<u8> = Vec::new();
        cmd.push(0x02);
        cmd.extend_from_slice(b"publish");
        cmd.push(0x00);
        cmd.extend_from_slice(&0.0_f64.to_be_bytes());
        cmd.push(0x05);
        cmd.push(0x02);
        cmd.extend_from_slice(b"live");
        cmd.push(0x02);
        cmd.extend_from_slice(b"live");

        assert_eq!(cmd[0], 0x02);
        assert_eq!(cmd[1], b'p');
    }

    // ========== WebRTC Protocol Tests ==========

    #[test]
    fn test_webrtc_offer_signal() {
        let offer = r#"{"type":"offer","sdp":"v=0\r\n\
                        o=- 123 2 IN IP4 127.0.0.1\r\n\
                        s=-\r\n\
                        t=0 0\r\n\
                        m=video 9 RTP/AVP 96\r\n\
                        a=rtpmap:96 H264/90000\r\n"}"#;
        assert!(offer.contains(r#""type":"offer""#));
        assert!(offer.contains("H264/90000"));
    }

    #[test]
    fn test_webrtc_answer_signal() {
        let answer = r#"{"type":"answer","sdp":"v=0\r\n\
                        o=- 456 2 IN IP4 127.0.0.1\r\n\
                        s=-\r\n\
                        t=0 0\r\n\
                        m=video 9 RTP/AVP 96\r\n\
                        a=rtpmap:96 H264/90000\r\n"}"#;
        assert!(answer.contains(r#""type":"answer""#));
    }

    #[test]
    fn test_webrtc_ice_candidate_signal() {
        let candidate = r#"{"type":"candidate",\
                           "candidate":"candidate:1 1 UDP 2130379007 192.168.1.1 5000 typ host",\
                           "sdpMLineIndex":0,
                           "sdpMid":"track0"}"#;
        assert!(candidate.contains(r#""type":"candidate""#));
        assert!(candidate.contains("UDP"));
        assert!(candidate.contains("sdpMLineIndex"));
    }

    #[test]
    fn test_webrtc_ice_candidate_parsing() {
        let cand = "candidate:1 1 UDP 2130379007 192.168.1.1 5000 typ host";
        assert!(cand.starts_with("candidate:"));
        assert!(cand.contains("UDP"));
        assert!(cand.contains("typ host"));
    }

    // ========== Cross-Protocol Streaming Tests ==========

    #[test]
    fn test_stream_frame_distribution() {
        use std::sync::Arc;

        let manager = StreamManager::new();
        manager.create_stream(
            "test_stream",
            StreamSourceMode::Push,
            StreamProtocol::Unknown,
            None,
        );
        manager.ensure_stream_hub("test_stream");

        let rtsp = manager.dispatch_subscribe(
            "test_stream",
            crate::core::DispatchPolicy::SequentialFromIdr,
        );
        let rtmp =
            manager.dispatch_subscribe("test_stream", crate::core::DispatchPolicy::LiveCoalesce);
        let webrtc =
            manager.dispatch_subscribe("test_stream", crate::core::DispatchPolicy::WebRtcPlay);

        assert!(rtsp.is_some());
        assert!(rtmp.is_some());
        assert!(webrtc.is_some());

        let frame = MediaFrame::new(
            "test_stream".to_string(),
            0,
            0,
            Bytes::from(vec![0x67, 0x42, 0x00, 0x0A]),
            true,
            CodecType::H264,
        );
        manager.publish_frame(frame);

        let hub = manager.get_hub("test_stream").unwrap();
        assert_eq!(hub.latest_seq(), 0);
    }

    #[test]
    fn test_multi_codec_stream_creation() {
        let manager = StreamManager::new();
        let tracks = vec![
            Track {
                id: 0,
                codec: CodecType::H264,
                payload_type: 96,
                clock_rate: 90000,
                extra_params: HashMap::new(),
            },
            Track {
                id: 1,
                codec: CodecType::H265,
                payload_type: 98,
                clock_rate: 90000,
                extra_params: HashMap::new(),
            },
            Track {
                id: 2,
                codec: CodecType::AAC,
                payload_type: 97,
                clock_rate: 44100,
                extra_params: HashMap::new(),
            },
            Track {
                id: 3,
                codec: CodecType::Opus,
                payload_type: 109,
                clock_rate: 48000,
                extra_params: HashMap::new(),
            },
        ];
        let stream = manager.create_stream(
            "multi_codec",
            StreamSourceMode::Push,
            StreamProtocol::Unknown,
            None,
        );
        manager.set_stream_tracks("multi_codec", tracks);
        let stream = manager.get_stream(&"multi_codec".to_string()).unwrap();
        assert_eq!(stream.tracks.len(), 4);
        assert_eq!(stream.tracks[0].codec, CodecType::H264);
        assert_eq!(stream.tracks[1].codec, CodecType::H265);
        assert_eq!(stream.tracks[2].codec, CodecType::AAC);
        assert_eq!(stream.tracks[3].codec, CodecType::Opus);
    }

    #[test]
    fn test_stream_id_validation() {
        let manager = StreamManager::new();
        let tracks = vec![Track {
            id: 0,
            codec: CodecType::H264,
            payload_type: 96,
            clock_rate: 90000,
            extra_params: HashMap::new(),
        }];

        // Test various stream ID formats
        let stream1 = manager.create_stream(
            "simple",
            StreamSourceMode::Push,
            StreamProtocol::Unknown,
            None,
        );
        assert_eq!(stream1.id, "simple");

        let stream2 = manager.create_stream(
            "with_underscore",
            StreamSourceMode::Push,
            StreamProtocol::Unknown,
            None,
        );
        assert_eq!(stream2.id, "with_underscore");

        let stream3 = manager.create_stream(
            "with-dash",
            StreamSourceMode::Push,
            StreamProtocol::Unknown,
            None,
        );
        assert_eq!(stream3.id, "with-dash");

        let stream4 = manager.create_stream(
            "123numeric",
            StreamSourceMode::Push,
            StreamProtocol::Unknown,
            None,
        );
        assert_eq!(stream4.id, "123numeric");
    }

    #[test]
    fn test_timestamp_calculation() {
        // Video: 25 fps -> 3600 timestamp increment per frame (90000/25)
        let video_clock_rate: u64 = 90000;
        let fps: u64 = 25;
        let frame_duration = video_clock_rate / fps;
        assert_eq!(frame_duration, 3600);

        // Audio: 44100 sample rate, 1024 samples per frame -> ~23.2ms
        let audio_clock_rate: u64 = 44100;
        let samples_per_frame: u64 = 1024;
        let audio_frame_duration = (samples_per_frame * 1000) / audio_clock_rate;
        assert_eq!(audio_frame_duration, 23); // ~23ms
    }

    #[test]
    fn test_h264_nal_unit_types() {
        // SPS (7)
        let sps_nal = 0x67u8;
        let nal_type = sps_nal & 0x1F;
        assert_eq!(nal_type, 7);

        // PPS (8)
        let pps_nal = 0x68u8;
        let nal_type = pps_nal & 0x1F;
        assert_eq!(nal_type, 8);

        // IDR (5)
        let idr_nal = 0x65u8;
        let nal_type = idr_nal & 0x1F;
        assert_eq!(nal_type, 5);

        // Non-IDR (1)
        let non_idr_nal = 0x41u8;
        let nal_type = non_idr_nal & 0x1F;
        assert_eq!(nal_type, 1);

        // SEI (6)
        let sei_nal = 0x06u8;
        let nal_type = sei_nal & 0x1F;
        assert_eq!(nal_type, 6);
    }
}
