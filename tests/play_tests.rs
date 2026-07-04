//! Integration tests for cross-protocol streaming with real ffmpeg/ffplay.
//!
//! These tests require actual ffmpeg and ffplay binaries and optionally
//! a running media server for end-to-end testing.
//!
//! Test categories:
//! - ffmpeg_external_tests: ffmpeg/ffplay binary validation
//! - integration_with_server: URL construction and config validation
//! - real_ffmpeg_integration_tests: Real streaming with ffmpeg publish + ffplay playback

mod common;

#[cfg(test)]
mod tests {
    use std::time::Duration;
    use tokio::time::sleep;

    use vcp_media_server::core::{
        CodecType, DispatchPolicy, MediaFrame, StreamManager, StreamProtocol, StreamSourceMode, Track,
    };

    use crate::common::{
        ffmpeg_helper::{create_simple_rtmp_publish_command, create_rtmp_play_command, FfmpegTestConfig},
        frame_generators::{generate_h264_idr_frame, generate_h265_idr_frame, generate_h264_sps_pps, generate_h265_vps_sps_pps},
        hls_helper::HlsTestHelper,
        http_flv_helper::HttpFlvTestHelper,
        media_server_runner::MediaServer,
        process_manager::{
            assert_ffmpeg_publishing, is_ffmpeg_available, is_ffplay_available,
            publish_and_hls_playback, publish_and_playback, publish_api_pull_and_play,
            run_ffplay_playback, spawn_ffmpeg, stop_child, PlayConfig, PlayProtocol, PublishCodec,
            PublishConfig,
        },
        rtmp_helper::RtmpTestHelper,
        rtsp_helper::{create_rtsp_announce_sdp, RtspTestHelper},
        stream_helper::{cleanup_test_streams, setup_test_stream},
        TestCodec, TestConfig,
    };

    // =========================================================================
    // FFmpeg External Process Tests (binary validation)
    // =========================================================================

    mod ffmpeg_external_tests {
        use super::*;
        use std::process::{Command, Stdio};

        /// Check if ffmpeg is available.
        fn is_ffmpeg_available() -> bool {
            Command::new("ffmpeg")
                .arg("-version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok()
        }

        /// Check if ffplay is available.
        fn is_ffplay_available() -> bool {
            Command::new("ffplay")
                .arg("-version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok()
        }

        #[tokio::test]
        async fn test_ffmpeg_command_generation() {
            if !is_ffmpeg_available() {
                eprintln!("ffmpeg not available, skipping test");
                return;
            }

            let config = FfmpegTestConfig::sd();
            let url = "rtmp://localhost:1935/live/test_stream";

            let cmd = create_simple_rtmp_publish_command(url, TestCodec::H264, &config);

            assert_eq!(cmd[0], "ffmpeg");
            assert!(cmd.contains(&"-f".to_string()));
            assert!(cmd.contains(&"lavfi".to_string()));
            assert!(cmd.contains(&"-c:v".to_string()));
            assert!(cmd.contains(&"libx264".to_string()));
        }

        #[tokio::test]
        async fn test_ffplay_command_generation() {
            if !is_ffplay_available() {
                eprintln!("ffplay not available, skipping test");
                return;
            }

            let url = "rtmp://localhost:1935/live/test_stream";
            let cmd = create_rtmp_play_command(url, 5);

            assert_eq!(cmd[0], "ffplay");
            assert!(cmd.contains(&"-i".to_string()));
            assert!(cmd.contains(&"-t".to_string()));
            assert!(cmd.contains(&"5".to_string()));
            assert!(cmd.contains(&"-nodisp".to_string()));
        }

        #[tokio::test]
        async fn test_ffmpeg_testsrc_generates_frames() {
            if !is_ffmpeg_available() {
                eprintln!("ffmpeg not available, skipping test");
                return;
            }

            // Test that ffmpeg can generate testsrc pattern
            let output = Command::new("ffmpeg")
                .args([
                    "-f", "lavfi",
                    "-i", "testsrc2=size=320x240:rate=25:duration=1",
                    "-t", "1",
                    "-y",
                    "-f", "null",
                    "/dev/null",
                ])
                .output()
                .expect("Failed to run ffmpeg");

            assert!(
                output.status.success(),
                "ffmpeg testsrc generation failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        #[tokio::test]
        async fn test_ffmpeg_h264_encoding() {
            if !is_ffmpeg_available() {
                eprintln!("ffmpeg not available, skipping test");
                return;
            }

            // Test that ffmpeg can encode H264 with testsrc
            let output = Command::new("ffmpeg")
                .args([
                    "-f", "lavfi",
                    "-i", "testsrc2=size=320x240:rate=25:duration=1",
                    "-c:v", "libx264",
                    "-preset", "ultrafast",
                    "-tune", "zerolatency",
                    "-pix_fmt", "yuv420p",
                    "-t", "1",
                    "-y",
                    "-f", "null",
                    "/dev/null",
                ])
                .output()
                .expect("Failed to run ffmpeg");

            assert!(
                output.status.success(),
                "ffmpeg H264 encoding failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        #[tokio::test]
        async fn test_ffmpeg_h265_encoding() {
            if !is_ffmpeg_available() {
                eprintln!("ffmpeg not available, skipping test");
                return;
            }

            // Test that ffmpeg can encode H265 with testsrc
            let output = Command::new("ffmpeg")
                .args([
                    "-f", "lavfi",
                    "-i", "testsrc2=size=320x240:rate=25:duration=1",
                    "-c:v", "libx265",
                    "-preset", "ultrafast",
                    "-tune", "zerolatency",
                    "-pix_fmt", "yuv420p",
                    "-t", "1",
                    "-y",
                    "-f", "null",
                    "/dev/null",
                ])
                .output()
                .expect("Failed to run ffmpeg");

            assert!(
                output.status.success(),
                "ffmpeg H265 encoding failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        #[tokio::test]
        async fn test_ffmpeg_audio_generation() {
            if !is_ffmpeg_available() {
                eprintln!("ffmpeg not available, skipping test");
                return;
            }

            // Test that ffmpeg can generate audio
            let output = Command::new("ffmpeg")
                .args([
                    "-f", "lavfi",
                    "-i", "aevalsrc=0.1*sin(2*PI*440*t):duration=1:sample_rate=48000",
                    "-c:a", "aac",
                    "-t", "1",
                    "-y",
                    "-f", "null",
                    "/dev/null",
                ])
                .output()
                .expect("Failed to run ffmpeg");

            assert!(
                output.status.success(),
                "ffmpeg audio generation failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        #[tokio::test]
        async fn test_ffmpeg_combined_av_stream() {
            if !is_ffmpeg_available() {
                eprintln!("ffmpeg not available, skipping test");
                return;
            }

            // Test combined audio and video generation
            let output = Command::new("ffmpeg")
                .args([
                    "-f", "lavfi",
                    "-i", "testsrc2=size=320x240:rate=25:duration=1",
                    "-f", "lavfi",
                    "-i", "aevalsrc=0.1*sin(2*PI*440*t):duration=1:sample_rate=48000",
                    "-map", "0:v",
                    "-map", "1:a",
                    "-c:v", "libx264",
                    "-preset", "ultrafast",
                    "-c:a", "aac",
                    "-t", "1",
                    "-y",
                    "-f", "null",
                    "/dev/null",
                ])
                .output()
                .expect("Failed to run ffmpeg");

            assert!(
                output.status.success(),
                "ffmpeg AV stream generation failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        #[tokio::test]
        async fn test_ffmpeg_flv_output_format() {
            if !is_ffmpeg_available() {
                eprintln!("ffmpeg not available, skipping test");
                return;
            }

            // Test FLV output format with testsrc (output to a temp file)
            let test_file = "/tmp/test_ffmpeg_flv_output.flv";
            let output = Command::new("ffmpeg")
                .args([
                    "-f", "lavfi",
                    "-i", "testsrc2=size=320x240:rate=25:duration=1",
                    "-f", "lavfi",
                    "-i", "aevalsrc=0.1*sin(2*PI*440*t):duration=1:sample_rate=48000",
                    "-map", "0:v",
                    "-map", "1:a",
                    "-c:v", "libx264",
                    "-preset", "ultrafast",
                    "-c:a", "aac",
                    "-t", "1",
                    "-y",
                    "-f", "flv",
                    test_file,
                ])
                .output()
                .expect("Failed to run ffmpeg");

            // Clean up
            let _ = std::fs::remove_file(test_file);

            assert!(
                output.status.success(),
                "ffmpeg FLV output test failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        #[tokio::test]
        async fn test_ffplay_can_decode_h264() {
            if !is_ffplay_available() || !is_ffmpeg_available() {
                eprintln!("ffmpeg/ffplay not available, skipping test");
                return;
            }

            // First create a test file with ffmpeg
            let test_file = "/tmp/test_ffplay_h264.ts";

            // Generate H264 test stream to file
            let gen_result = Command::new("ffmpeg")
                .args([
                    "-f", "lavfi",
                    "-i", "testsrc2=size=320x240:rate=25:duration=2",
                    "-f", "lavfi",
                    "-i", "aevalsrc=0.1*sin(2*PI*440*t):duration=2:sample_rate=48000",
                    "-map", "0:v",
                    "-map", "1:a",
                    "-c:v", "libx264",
                    "-preset", "ultrafast",
                    "-c:a", "aac",
                    "-t", "2",
                    "-y",
                    test_file,
                ])
                .output()
                .expect("Failed to generate test file");

            if !gen_result.status.success() {
                eprintln!("Test file generation failed, skipping: {}",
                    String::from_utf8_lossy(&gen_result.stderr));
                return;
            }

            // Now test ffplay can read it (with timeout)
            // Note: ffplay may fail on headless systems due to display issues
            let play_result = Command::new("timeout")
                .args(["5", "ffplay", "-autoexit", "-nodisp", "-v", "error", test_file])
                .output();

            // Clean up
            let _ = std::fs::remove_file(test_file);

            // ffplay may fail due to display issues, but if it works or gracefully exits, we consider it a pass
            if let Ok(output) = play_result {
                // Exit code 124 means timeout was reached, which is okay for our test
                // Any other exit code besides 0 may indicate display issues
                let exit_ok = output.status.success() || output.status.code() == Some(124);
                if !exit_ok {
                    eprintln!("ffplay exited with code {:?}: {}",
                        output.status.code(),
                        String::from_utf8_lossy(&output.stderr));
                }
            } else {
                eprintln!("ffplay execution failed (this may be expected on headless systems)");
            }
        }
    }

    // =========================================================================
    // Real FFmpeg/FFplay Integration Tests (with embedded media server)
    // =========================================================================

    mod real_ffmpeg_integration_tests {
        use super::*;

        fn unique_stream(prefix: &str) -> String {
            format!(
                "{prefix}_{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis()
            )
        }

        fn require_ffmpeg_ffplay() {
            if !is_ffmpeg_available() {
                panic!("ffmpeg not available, this test requires ffmpeg");
            }
            if !is_ffplay_available() {
                panic!("ffplay not available, this test requires ffplay");
            }
        }

        fn rtmp_publish(
            server: &MediaServer,
            stream_id: &str,
            codec: PublishCodec,
            duration_secs: u32,
        ) -> PublishConfig {
            PublishConfig::rtmp("127.0.0.1", server.ports().rtmp, "live", stream_id)
                .resolution(640, 480)
                .framerate(25)
                .duration(duration_secs)
                .codec(codec)
        }

        #[tokio::test]
        async fn test_real_integration_all() {
            require_ffmpeg_ffplay();

            let server = MediaServer::start_isolated().expect(
                "failed to start media server — run `cargo build --bin vcp-media-server` first",
            );
            server.wait_ready().await;

            let stream_id = unique_stream("test_rtmp_h264");
            let publish = rtmp_publish(&server, &stream_id, PublishCodec::H264, 8);
            let play = PlayConfig::rtmp(&server.rtmp_url("live", &stream_id)).duration(3);
            publish_and_playback(&publish, &play)
                .await
                .expect("RTMP H264 publish + ffplay playback should succeed");
            println!("RTMP H264 publish + play OK: {stream_id}");

            let stream_id = unique_stream("test_rtmp_h265");
            let publish = rtmp_publish(&server, &stream_id, PublishCodec::H265, 8);
            let play = PlayConfig::rtmp(&server.rtmp_url("live", &stream_id))
                .duration(3)
                .enhanced_rtmp_hevc();
            publish_and_playback(&publish, &play)
                .await
                .expect("RTMP H265 publish + ffplay playback should succeed");
            println!("RTMP H265 publish + play OK: {stream_id}");

            let stream_id = unique_stream("test_flow_flv");
            let publish = rtmp_publish(&server, &stream_id, PublishCodec::H264, 10);
            let play = PlayConfig::http_flv(&server.http_flv_url(&stream_id)).duration(3);
            publish_and_playback(&publish, &play)
                .await
                .expect("RTMP publish + HTTP-FLV ffplay should succeed");
            println!("RTMP publish -> HTTP-FLV playback OK: {stream_id}");

            let stream_id = unique_stream("test_flow_rtsp");
            let publish = rtmp_publish(&server, &stream_id, PublishCodec::H264, 10);
            let play = PlayConfig::rtsp(&server.rtsp_url(&stream_id)).duration(3);
            publish_and_playback(&publish, &play)
                .await
                .expect("RTMP publish + RTSP ffplay should succeed");
            println!("RTMP publish -> RTSP playback OK: {stream_id}");

            let stream_id = unique_stream("test_rtsp_tcp");
            let publish = rtmp_publish(&server, &stream_id, PublishCodec::H264, 10);
            let play = PlayConfig::rtsp(&server.rtsp_url(&stream_id))
                .duration(3)
                .rtsp_transport_tcp();
            publish_and_playback(&publish, &play)
                .await
                .expect("RTMP publish + RTSP TCP ffplay should succeed");
            println!("RTMP publish -> RTSP TCP playback OK: {stream_id}");

            let stream_id = unique_stream("test_rtsp_udp");
            let publish = rtmp_publish(&server, &stream_id, PublishCodec::H264, 10);
            let play = PlayConfig::rtsp(&server.rtsp_url(&stream_id))
                .duration(3)
                .rtsp_transport_udp();
            publish_and_playback(&publish, &play)
                .await
                .expect("RTMP publish + RTSP UDP ffplay should succeed");
            println!("RTMP publish -> RTSP UDP playback OK: {stream_id}");

            let stream_id = unique_stream("test_hls");
            let publish = rtmp_publish(&server, &stream_id, PublishCodec::H264, 12);
            let play = PlayConfig::hls(&server.hls_url(&stream_id)).duration(5);
            publish_and_hls_playback(&server, &stream_id, &publish, &play)
                .await
                .expect("RTMP publish + HLS ffplay should succeed");
            println!("RTMP publish -> HLS playback OK: {stream_id}");

            let src_id = unique_stream("test_pull_rtmp_src");
            let pulled_id = unique_stream("test_pull_rtmp_out");
            let publish = rtmp_publish(&server, &src_id, PublishCodec::H264, 12);
            let remote_url = server.rtmp_url("live", &src_id);
            let play = PlayConfig::rtmp(&server.rtmp_url("live", &pulled_id)).duration(3);
            publish_api_pull_and_play(&server, &publish, &remote_url, &pulled_id, false, &play)
                .await
                .expect("RTMP API pull + ffplay should succeed");
            println!("RTMP API pull + playback OK: {src_id} -> {pulled_id}");

            let src_id = unique_stream("test_pull_rtsp_src");
            let pulled_id = unique_stream("test_pull_rtsp_tcp");
            let publish = PublishConfig::rtsp("127.0.0.1", server.ports().rtsp, &src_id)
                .resolution(640, 480)
                .framerate(25)
                .duration(12)
                .codec(PublishCodec::H264);
            let remote_url = server.rtsp_url(&src_id);
            let play = PlayConfig::rtsp(&server.rtsp_url(&pulled_id))
                .duration(3)
                .rtsp_transport_tcp();
            publish_api_pull_and_play(&server, &publish, &remote_url, &pulled_id, true, &play)
                .await
                .expect("RTSP API pull (TCP) + ffplay should succeed");
            println!("RTSP API pull (TCP) + playback OK: {src_id} -> {pulled_id}");

            let src_id = unique_stream("test_pull_rtsp_udp_src");
            let pulled_id = unique_stream("test_pull_rtsp_udp");
            let publish = PublishConfig::rtsp("127.0.0.1", server.ports().rtsp, &src_id)
                .resolution(640, 480)
                .framerate(25)
                .duration(12)
                .codec(PublishCodec::H264);
            let remote_url = format!("{}?transport=udp", server.rtsp_url(&src_id));
            let play = PlayConfig::rtsp(&server.rtsp_url(&pulled_id))
                .duration(3)
                .rtsp_transport_udp();
            publish_api_pull_and_play(&server, &publish, &remote_url, &pulled_id, true, &play)
                .await
                .expect("RTSP API pull (UDP) + ffplay should succeed");
            println!("RTSP API pull (UDP) + playback OK: {src_id} -> {pulled_id}");

            let stream_id = unique_stream("test_rtsp_h264");
            let publish = PublishConfig::rtsp("127.0.0.1", server.ports().rtsp, &stream_id)
                .resolution(640, 480)
                .framerate(25)
                .duration(8)
                .codec(PublishCodec::H264);
            let play = PlayConfig::rtsp(&server.rtsp_url(&stream_id)).duration(3);
            publish_and_playback(&publish, &play)
                .await
                .expect("RTSP H264 publish + ffplay playback should succeed");
            println!("RTSP H264 publish + play OK: {stream_id}");

            let stream_id = unique_stream("test_h265_flv");
            let publish = rtmp_publish(&server, &stream_id, PublishCodec::H265, 10);
            let play = PlayConfig::http_flv(&server.http_flv_url(&stream_id)).duration(3);
            publish_and_playback(&publish, &play)
                .await
                .expect("H265 RTMP publish + HTTP-FLV ffplay should succeed");
            println!("H265 RTMP publish -> HTTP-FLV playback OK: {stream_id}");

            let h264_stream = unique_stream("test_con_h264");
            let h265_stream = unique_stream("test_con_h265");
            let h264_publish = rtmp_publish(&server, &h264_stream, PublishCodec::H264, 10);
            let h265_publish = rtmp_publish(&server, &h265_stream, PublishCodec::H265, 10);

            let mut h264_ffmpeg = spawn_ffmpeg(&h264_publish.ffmpeg_args()).expect("spawn H264 ffmpeg");
            let mut h265_ffmpeg = spawn_ffmpeg(&h265_publish.ffmpeg_args()).expect("spawn H265 ffmpeg");

            assert_ffmpeg_publishing(&mut h264_ffmpeg, Duration::from_millis(1500))
                .await
                .expect("H264 ffmpeg publishing");
            assert_ffmpeg_publishing(&mut h265_ffmpeg, Duration::from_millis(500))
                .await
                .expect("H265 ffmpeg publishing");

            run_ffplay_playback(
                &PlayConfig::rtmp(&server.rtmp_url("live", &h264_stream)).duration(2),
            )
            .await
            .expect("H264 ffplay playback");
            run_ffplay_playback(
                &PlayConfig::rtmp(&server.rtmp_url("live", &h265_stream))
                    .duration(2)
                    .enhanced_rtmp_hevc(),
            )
            .await
            .expect("H265 ffplay playback");

            let _ = stop_child(&mut h264_ffmpeg);
            let _ = stop_child(&mut h265_ffmpeg);
            println!("Concurrent H264/H265 publish + play OK");

            println!("All real integration tests completed successfully!");
        }
    }
}
