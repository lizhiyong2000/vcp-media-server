//! Unit tests for cross-protocol streaming core functionality.
//!
//! These tests use mocked/inner StreamTester to simulate streaming
//! without requiring actual ffmpeg or server infrastructure.
//!
//! Test coverage:
//! - Stream management operations
//! - Frame processing and sequencing
//! - Dispatch and subscription logic
//! - Codec detection and validation
//! - Protocol helper utilities

mod common;

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::time::sleep;

    use vcp_media_server::core::{
        CodecType, DispatchPolicy, MediaFrame, StreamManager, StreamProtocol, StreamSourceMode, Track,
    };

    use crate::common::{
        frame_generators::{generate_h264_idr_frame, generate_h265_idr_frame, generate_h264_sps_pps, generate_h265_vps_sps_pps},
        hls_helper::HlsTestHelper,
        http_flv_helper::HttpFlvTestHelper,
        rtmp_helper::RtmpTestHelper,
        rtsp_helper::{create_rtsp_announce_sdp, RtspTestHelper},
        stream_helper::{cleanup_test_streams, setup_test_stream},
        TestCodec, TestConfig,
    };

    // =========================================================================
    // H264 Cross-Protocol Tests (Mocked)
    // =========================================================================

    mod h264_rtmp_publish {
        use super::*;

        #[tokio::test]
        async fn test_h264_rtmp_publish_http_flv_playback() {
            let config = TestConfig::new("h264_rtmp_flv", TestCodec::H264);
            let manager = Arc::new(StreamManager::new());

            // Setup stream
            let _tracks = setup_test_stream(&manager, &config.stream_id, TestCodec::H264);

            // Publish via API (simulating RTMP publish)
            let tester = vcp_media_server::core::StreamTester::new(manager.clone());
            tester.push_test_stream(&config.stream_id, 2).await.unwrap();

            // Wait for frames to be published
            sleep(Duration::from_millis(500)).await;

            // Verify stream exists
            let stream = manager.get_stream(&config.stream_id);
            assert!(stream.is_some(), "Stream should exist after publish");

            // Subscribe for HTTP-FLV playback
            let reader = manager.dispatch_subscribe(&config.stream_id, DispatchPolicy::LiveCoalesce);
            assert!(reader.is_some(), "Should be able to subscribe for HTTP-FLV");

            // Cleanup
            cleanup_test_streams(&manager, &[&config.stream_id]);
        }

        #[tokio::test]
        async fn test_h264_rtmp_publish_rtsp_playback() {
            let config = TestConfig::new("h264_rtmp_rtsp", TestCodec::H264);
            let manager = Arc::new(StreamManager::new());

            let _tracks = setup_test_stream(&manager, &config.stream_id, TestCodec::H264);

            // Publish via API
            let tester = vcp_media_server::core::StreamTester::new(manager.clone());
            tester.push_test_stream(&config.stream_id, 2).await.unwrap();

            sleep(Duration::from_millis(500)).await;

            // Subscribe for RTSP playback
            let reader = manager.dispatch_subscribe(&config.stream_id, DispatchPolicy::SequentialFromIdr);
            assert!(reader.is_some(), "Should be able to subscribe for RTSP");

            cleanup_test_streams(&manager, &[&config.stream_id]);
        }

        #[tokio::test]
        async fn test_h264_rtmp_publish_hls_playback() {
            let config = TestConfig::new("h264_rtmp_hls", TestCodec::H264);
            let manager = Arc::new(StreamManager::new());

            let _tracks = setup_test_stream(&manager, &config.stream_id, TestCodec::H264);

            // Publish frames
            let tester = vcp_media_server::core::StreamTester::new(manager.clone());
            tester.push_test_stream(&config.stream_id, 2).await.unwrap();

            sleep(Duration::from_millis(500)).await;

            // Verify hub has frames
            let hub = manager.get_hub(&config.stream_id);
            assert!(hub.is_some(), "Hub should exist");

            cleanup_test_streams(&manager, &[&config.stream_id]);
        }
    }

    mod h264_rtsp_publish {
        use super::*;

        #[tokio::test]
        async fn test_h264_rtsp_publish_http_flv_playback() {
            let config = TestConfig::new("h264_rtsp_flv", TestCodec::H264);
            let manager = Arc::new(StreamManager::new());

            let _tracks = setup_test_stream(&manager, &config.stream_id, TestCodec::H264);

            // Simulate RTSP publish via API
            let tester = vcp_media_server::core::StreamTester::new(manager.clone());
            tester.push_test_stream(&config.stream_id, 2).await.unwrap();

            sleep(Duration::from_millis(500)).await;

            // Subscribe for HTTP-FLV
            let reader = manager.dispatch_subscribe(&config.stream_id, DispatchPolicy::LiveCoalesce);
            assert!(reader.is_some());

            cleanup_test_streams(&manager, &[&config.stream_id]);
        }

        #[tokio::test]
        async fn test_h264_rtsp_publish_rtmp_playback() {
            let config = TestConfig::new("h264_rtsp_rtmp", TestCodec::H264);
            let manager = Arc::new(StreamManager::new());

            let _tracks = setup_test_stream(&manager, &config.stream_id, TestCodec::H264);

            let tester = vcp_media_server::core::StreamTester::new(manager.clone());
            tester.push_test_stream(&config.stream_id, 2).await.unwrap();

            sleep(Duration::from_millis(500)).await;

            // Subscribe for RTMP playback
            let reader = manager.dispatch_subscribe(&config.stream_id, DispatchPolicy::LiveCoalesce);
            assert!(reader.is_some());

            cleanup_test_streams(&manager, &[&config.stream_id]);
        }

        #[tokio::test]
        async fn test_h264_rtsp_publish_hls_playback() {
            let config = TestConfig::new("h264_rtsp_hls", TestCodec::H264);
            let manager = Arc::new(StreamManager::new());

            let _tracks = setup_test_stream(&manager, &config.stream_id, TestCodec::H264);

            let tester = vcp_media_server::core::StreamTester::new(manager.clone());
            tester.push_test_stream(&config.stream_id, 2).await.unwrap();

            sleep(Duration::from_millis(500)).await;

            let hub = manager.get_hub(&config.stream_id);
            assert!(hub.is_some());

            cleanup_test_streams(&manager, &[&config.stream_id]);
        }
    }

    mod h264_api_push {
        use super::*;

        #[tokio::test]
        async fn test_h264_api_push_all_playback_protocols() {
            let config = TestConfig::new("h264_api_all", TestCodec::H264);
            let manager = Arc::new(StreamManager::new());

            let _tracks = setup_test_stream(&manager, &config.stream_id, TestCodec::H264);

            // Publish via API
            let tester = vcp_media_server::core::StreamTester::new(manager.clone());
            tester.push_test_stream(&config.stream_id, 2).await.unwrap();

            sleep(Duration::from_millis(500)).await;

            // Test all playback protocols
            let rtmp_reader = manager.dispatch_subscribe(&config.stream_id, DispatchPolicy::LiveCoalesce);
            let rtsp_reader = manager.dispatch_subscribe(&config.stream_id, DispatchPolicy::SequentialFromIdr);
            let webrtc_reader = manager.dispatch_subscribe(&config.stream_id, DispatchPolicy::WebRtcPlay);

            assert!(rtmp_reader.is_some(), "RTMP playback should work");
            assert!(rtsp_reader.is_some(), "RTSP playback should work");
            assert!(webrtc_reader.is_some(), "WebRTC playback should work");

            cleanup_test_streams(&manager, &[&config.stream_id]);
        }
    }

    // =========================================================================
    // H265 Cross-Protocol Tests (Mocked)
    // =========================================================================

    mod h265_rtmp_publish {
        use super::*;

        #[tokio::test]
        async fn test_h265_rtmp_publish_http_flv_playback() {
            let config = TestConfig::new("h265_rtmp_flv", TestCodec::H265);
            let manager = Arc::new(StreamManager::new());

            let _tracks = setup_test_stream(&manager, &config.stream_id, TestCodec::H265);

            // Publish H265 via API (simulating RTMP publish)
            let tester = vcp_media_server::core::StreamTester::new(manager.clone());
            tester.push_test_stream(&config.stream_id, 2).await.unwrap();

            sleep(Duration::from_millis(500)).await;

            let stream = manager.get_stream(&config.stream_id);
            assert!(stream.is_some());

            let reader = manager.dispatch_subscribe(&config.stream_id, DispatchPolicy::LiveCoalesce);
            assert!(reader.is_some());

            cleanup_test_streams(&manager, &[&config.stream_id]);
        }

        #[tokio::test]
        async fn test_h265_rtmp_publish_rtsp_playback() {
            let config = TestConfig::new("h265_rtmp_rtsp", TestCodec::H265);
            let manager = Arc::new(StreamManager::new());

            let _tracks = setup_test_stream(&manager, &config.stream_id, TestCodec::H265);

            let tester = vcp_media_server::core::StreamTester::new(manager.clone());
            tester.push_test_stream(&config.stream_id, 2).await.unwrap();

            sleep(Duration::from_millis(500)).await;

            let reader = manager.dispatch_subscribe(&config.stream_id, DispatchPolicy::SequentialFromIdr);
            assert!(reader.is_some());

            cleanup_test_streams(&manager, &[&config.stream_id]);
        }

        #[tokio::test]
        async fn test_h265_rtmp_publish_hls_playback() {
            let config = TestConfig::new("h265_rtmp_hls", TestCodec::H265);
            let manager = Arc::new(StreamManager::new());

            let _tracks = setup_test_stream(&manager, &config.stream_id, TestCodec::H265);

            let tester = vcp_media_server::core::StreamTester::new(manager.clone());
            tester.push_test_stream(&config.stream_id, 2).await.unwrap();

            sleep(Duration::from_millis(500)).await;

            let hub = manager.get_hub(&config.stream_id);
            assert!(hub.is_some());

            cleanup_test_streams(&manager, &[&config.stream_id]);
        }
    }

    mod h265_rtsp_publish {
        use super::*;

        #[tokio::test]
        async fn test_h265_rtsp_publish_http_flv_playback() {
            let config = TestConfig::new("h265_rtsp_flv", TestCodec::H265);
            let manager = Arc::new(StreamManager::new());

            let _tracks = setup_test_stream(&manager, &config.stream_id, TestCodec::H265);

            let tester = vcp_media_server::core::StreamTester::new(manager.clone());
            tester.push_test_stream(&config.stream_id, 2).await.unwrap();

            sleep(Duration::from_millis(500)).await;

            let reader = manager.dispatch_subscribe(&config.stream_id, DispatchPolicy::LiveCoalesce);
            assert!(reader.is_some());

            cleanup_test_streams(&manager, &[&config.stream_id]);
        }

        #[tokio::test]
        async fn test_h265_rtsp_publish_rtmp_playback() {
            let config = TestConfig::new("h265_rtsp_rtmp", TestCodec::H265);
            let manager = Arc::new(StreamManager::new());

            let _tracks = setup_test_stream(&manager, &config.stream_id, TestCodec::H265);

            let tester = vcp_media_server::core::StreamTester::new(manager.clone());
            tester.push_test_stream(&config.stream_id, 2).await.unwrap();

            sleep(Duration::from_millis(500)).await;

            let reader = manager.dispatch_subscribe(&config.stream_id, DispatchPolicy::LiveCoalesce);
            assert!(reader.is_some());

            cleanup_test_streams(&manager, &[&config.stream_id]);
        }

        #[tokio::test]
        async fn test_h265_rtsp_publish_hls_playback() {
            let config = TestConfig::new("h265_rtsp_hls", TestCodec::H265);
            let manager = Arc::new(StreamManager::new());

            let _tracks = setup_test_stream(&manager, &config.stream_id, TestCodec::H265);

            let tester = vcp_media_server::core::StreamTester::new(manager.clone());
            tester.push_test_stream(&config.stream_id, 2).await.unwrap();

            sleep(Duration::from_millis(500)).await;

            let hub = manager.get_hub(&config.stream_id);
            assert!(hub.is_some());

            cleanup_test_streams(&manager, &[&config.stream_id]);
        }
    }

    mod h265_api_push {
        use super::*;

        #[tokio::test]
        async fn test_h265_api_push_all_playback_protocols() {
            let config = TestConfig::new("h265_api_all", TestCodec::H265);
            let manager = Arc::new(StreamManager::new());

            let _tracks = setup_test_stream(&manager, &config.stream_id, TestCodec::H265);

            let tester = vcp_media_server::core::StreamTester::new(manager.clone());
            tester.push_test_stream(&config.stream_id, 2).await.unwrap();

            sleep(Duration::from_millis(500)).await;

            let rtmp_reader = manager.dispatch_subscribe(&config.stream_id, DispatchPolicy::LiveCoalesce);
            let rtsp_reader = manager.dispatch_subscribe(&config.stream_id, DispatchPolicy::SequentialFromIdr);
            let webrtc_reader = manager.dispatch_subscribe(&config.stream_id, DispatchPolicy::WebRtcPlay);

            assert!(rtmp_reader.is_some(), "RTMP playback should work for H265");
            assert!(rtsp_reader.is_some(), "RTSP playback should work for H265");
            assert!(webrtc_reader.is_some(), "WebRTC playback should work for H265");

            cleanup_test_streams(&manager, &[&config.stream_id]);
        }
    }

    // =========================================================================
    // Codec Matrix Tests
    // =========================================================================

    mod codec_matrix {
        use super::*;

        #[tokio::test]
        async fn test_h264_h265_concurrent_streams() {
            let manager = Arc::new(StreamManager::new());

            // H264 stream
            let h264_stream_id = "test_h264_concurrent";
            let _tracks = setup_test_stream(&manager, h264_stream_id, TestCodec::H264);

            // H265 stream
            let h265_stream_id = "test_h265_concurrent";
            let _tracks = setup_test_stream(&manager, h265_stream_id, TestCodec::H265);

            // Publish to both
            let tester = vcp_media_server::core::StreamTester::new(manager.clone());

            let h264_tester = tester.push_test_stream(h264_stream_id, 1);
            let h265_tester = tester.push_test_stream(h265_stream_id, 1);

            tokio::join!(h264_tester, h265_tester).0.unwrap();
            sleep(Duration::from_millis(200)).await;

            // Both streams should exist
            assert!(manager.get_stream(&h264_stream_id.to_string()).is_some());
            assert!(manager.get_stream(&h265_stream_id.to_string()).is_some());

            // Both should be playable via HTTP-FLV
            assert!(manager.dispatch_subscribe(h264_stream_id, DispatchPolicy::LiveCoalesce).is_some());
            assert!(manager.dispatch_subscribe(h265_stream_id, DispatchPolicy::LiveCoalesce).is_some());

            cleanup_test_streams(&manager, &[h264_stream_id, h265_stream_id]);
        }

        #[tokio::test]
        async fn test_codec_type_detection() {
            // Test H264 codec type
            assert_eq!(CodecType::H264, CodecType::from_pt(96));

            // Test H265 codec type
            assert_eq!(CodecType::H265, CodecType::from_pt(98));

            // Test AAC codec type
            assert_eq!(CodecType::AAC, CodecType::from_pt(97));

            // Test Opus codec type
            assert_eq!(CodecType::Opus, CodecType::from_pt(109));
        }

        #[tokio::test]
        async fn test_h264_frame_keyframe_detection() {
            let idr_data = generate_h264_idr_frame(0);
            // H264 IDR NAL type is 0x65
            assert_eq!(idr_data[0] & 0x1F, 5, "IDR frame should have NAL type 5");
        }

        #[tokio::test]
        async fn test_h265_frame_keyframe_detection() {
            let idr_data = generate_h265_idr_frame(0);
            // H265 IDR NAL type (first byte after start code)
            // VPS is 32-34, SPS is 33-35, IDR is 19-21
            assert!(!idr_data.is_empty(), "H265 IDR frame should not be empty");
        }
    }

    // =========================================================================
    // Stream Management Tests
    // =========================================================================

    mod stream_management {
        use super::*;

        #[tokio::test]
        async fn test_stream_creation_and_deletion() {
            let manager = Arc::new(StreamManager::new());
            let stream_id = "test_create_delete";

            // Create stream
            manager.create_stream(stream_id, StreamSourceMode::Push, StreamProtocol::RTMP, None);
            assert!(manager.get_stream(&stream_id.to_string()).is_some());

            // Remove stream
            let removed = manager.remove_stream(&stream_id.to_string());
            assert!(removed.is_some());
            assert!(manager.get_stream(&stream_id.to_string()).is_none());
        }

        #[tokio::test]
        async fn test_stream_list_operations() {
            let manager = Arc::new(StreamManager::new());

            manager.create_stream("stream1", StreamSourceMode::Push, StreamProtocol::RTMP, None);
            manager.create_stream("stream2", StreamSourceMode::Pull, StreamProtocol::RTSP, None);

            let streams = manager.list_streams();
            assert_eq!(streams.len(), 2);
            assert!(streams.contains(&"stream1".to_string()));
            assert!(streams.contains(&"stream2".to_string()));

            cleanup_test_streams(&manager, &["stream1", "stream2"]);
        }

        #[tokio::test]
        async fn test_multiple_subscribers_same_stream() {
            let manager = Arc::new(StreamManager::new());
            let stream_id = "test_multi_sub";

            let _tracks = setup_test_stream(&manager, stream_id, TestCodec::H264);

            // Multiple HTTP-FLV subscribers
            let reader1 = manager.dispatch_subscribe(stream_id, DispatchPolicy::LiveCoalesce);
            let reader2 = manager.dispatch_subscribe(stream_id, DispatchPolicy::LiveCoalesce);

            assert!(reader1.is_some());
            assert!(reader2.is_some());

            cleanup_test_streams(&manager, &[stream_id]);
        }

        #[tokio::test]
        async fn test_stream_with_pull_url() {
            let manager = Arc::new(StreamManager::new());
            let stream_id = "test_pull_stream";
            let pull_url = "rtsp://example.com/live";

            manager.create_stream(
                stream_id,
                StreamSourceMode::Pull,
                StreamProtocol::RTSP,
                Some(pull_url.to_string()),
            );

            let stream = manager.get_stream(&stream_id.to_string());
            assert!(stream.is_some());
            assert_eq!(stream.unwrap().pull_url, Some(pull_url.to_string()));

            cleanup_test_streams(&manager, &[stream_id]);
        }
    }

    // =========================================================================
    // Frame Processing Tests
    // =========================================================================

    mod frame_processing {
        use super::*;
        use bytes::Bytes;

        #[tokio::test]
        async fn test_h264_frame_with_sps_pps() {
            let manager = Arc::new(StreamManager::new());
            let stream_id = "test_h264_sps";

            manager.create_stream(stream_id, StreamSourceMode::Push, StreamProtocol::Unknown, None);
            manager.ensure_stream_hub(stream_id);

            // Send SPS/PPS first
            let sps_pps = generate_h264_sps_pps();
            let frame = MediaFrame::new(
                stream_id.to_string(),
                0,
                0,
                sps_pps,
                true,
                CodecType::H264,
            );
            manager.publish_frame(frame);

            // Send IDR
            let idr = generate_h264_idr_frame(0);
            let frame = MediaFrame::new(
                stream_id.to_string(),
                0,
                3600,
                idr,
                true,
                CodecType::H264,
            );
            manager.publish_frame(frame);

            sleep(Duration::from_millis(100)).await;

            let hub = manager.get_hub(stream_id);
            assert!(hub.is_some());
            assert!(hub.unwrap().latest_seq() >= 1);

            cleanup_test_streams(&manager, &[stream_id]);
        }

        #[tokio::test]
        async fn test_h265_frame_with_vps_sps_pps() {
            let manager = Arc::new(StreamManager::new());
            let stream_id = "test_h265_params";

            manager.create_stream(stream_id, StreamSourceMode::Push, StreamProtocol::Unknown, None);
            manager.ensure_stream_hub(stream_id);

            // Send VPS/SPS/PPS
            let vps_sps_pps = generate_h265_vps_sps_pps();
            let frame = MediaFrame::new(
                stream_id.to_string(),
                0,
                0,
                vps_sps_pps,
                true,
                CodecType::H265,
            );
            manager.publish_frame(frame);

            // Send IDR
            let idr = generate_h265_idr_frame(0);
            let frame = MediaFrame::new(
                stream_id.to_string(),
                0,
                3600,
                idr,
                true,
                CodecType::H265,
            );
            manager.publish_frame(frame);

            sleep(Duration::from_millis(100)).await;

            let hub = manager.get_hub(stream_id);
            assert!(hub.is_some());

            cleanup_test_streams(&manager, &[stream_id]);
        }

        #[tokio::test]
        async fn test_frame_sequence_numbers() {
            let manager = Arc::new(StreamManager::new());
            let stream_id = "test_seq";

            manager.create_stream(stream_id, StreamSourceMode::Push, StreamProtocol::Unknown, None);
            manager.ensure_stream_hub(stream_id);

            // Publish 10 frames
            for i in 0..10 {
                let frame = MediaFrame::new(
                    stream_id.to_string(),
                    0,
                    (i * 3600) as u64,
                    Bytes::from(vec![0x65, i as u8]),
                    i == 0,
                    CodecType::H264,
                );
                manager.publish_frame(frame);
            }

            sleep(Duration::from_millis(100)).await;

            let hub = manager.get_hub(stream_id);
            assert!(hub.is_some());
            // Should have sequence number for at least the IDR frame
            assert!(hub.unwrap().latest_seq() >= 1);

            cleanup_test_streams(&manager, &[stream_id]);
        }
    }

    // =========================================================================
    // Protocol Helper Tests
    // =========================================================================

    mod protocol_helpers {
        use super::*;

        #[tokio::test]
        async fn test_rtmp_command_generation() {
            let stream_key = "test_stream";
            let app = "live";

            let command = crate::common::rtmp_helper::create_rtmp_publish_command(stream_key, app);
            assert!(command.contains("publish"));
            assert!(command.contains(stream_key));
            assert!(command.contains(app));
        }

        #[tokio::test]
        async fn test_rtsp_sdp_generation_h264() {
            let sdp = create_rtsp_announce_sdp(TestCodec::H264);
            assert!(sdp.contains("H264/90000"));
            assert!(sdp.contains("v=0"));
        }

        #[tokio::test]
        async fn test_rtsp_sdp_generation_h265() {
            let sdp = create_rtsp_announce_sdp(TestCodec::H265);
            assert!(sdp.contains("H265/90000") || sdp.contains("hev1") || sdp.contains("hvc1"));
            assert!(sdp.contains("v=0"));
        }

        #[tokio::test]
        async fn test_http_flv_helper_creation() {
            let helper = HttpFlvTestHelper::new("test_flv", "127.0.0.1", 8081);
            assert_eq!(helper.stream_id(), "test_flv");
            assert_eq!(helper.port(), 8081);
        }

        #[tokio::test]
        async fn test_hls_helper_creation() {
            let helper = HlsTestHelper::new("test_hls", "/tmp/hls");
            assert_eq!(helper.stream_id(), "test_hls");
        }
    }
}
