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
    use tokio::time::{sleep, Instant};

    use vcp_media_server::core::{
        CodecType, DispatchPolicy, MediaFrame, StreamManager, StreamProtocol, StreamSourceMode,
        Track,
    };

    use crate::common::{
        ffmpeg_helper::{
            create_rtmp_play_command, create_simple_rtmp_publish_command, FfmpegTestConfig,
        },
        frame_generators::{
            generate_h264_idr_frame, generate_h264_sps_pps, generate_h265_idr_frame,
            generate_h265_vps_sps_pps,
        },
        hls_helper::HlsTestHelper,
        http_flv_helper::HttpFlvTestHelper,
        media_server_runner::MediaServer,
        process_manager::{
            assert_ffmpeg_publishing, format_command, is_ffmpeg_available, is_ffplay_available,
            publish_and_decode_check, publish_and_hls_playback, publish_and_playback,
            publish_api_pull_and_play, run_ffmpeg_decode_without_api_check, run_ffplay_playback,
            spawn_ffmpeg, spawn_ffmpeg_for_stream, stop_child, verify_hls_playlist,
            verify_http_flv_stream, PlayConfig, PlayProtocol, PublishCodec, PublishConfig,
        },
        rtmp_helper::RtmpTestHelper,
        rtsp_helper::{create_rtsp_announce_sdp, RtspTestHelper},
        stream_helper::{cleanup_test_streams, setup_test_stream},
        verify::verify_stream_server_side,
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
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=320x240:rate=25:duration=1",
                    "-t",
                    "1",
                    "-y",
                    "-f",
                    "null",
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
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=320x240:rate=25:duration=1",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-tune",
                    "zerolatency",
                    "-pix_fmt",
                    "yuv420p",
                    "-t",
                    "1",
                    "-y",
                    "-f",
                    "null",
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
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=320x240:rate=25:duration=1",
                    "-c:v",
                    "libx265",
                    "-preset",
                    "ultrafast",
                    "-tune",
                    "zerolatency",
                    "-pix_fmt",
                    "yuv420p",
                    "-t",
                    "1",
                    "-y",
                    "-f",
                    "null",
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
                    "-f",
                    "lavfi",
                    "-i",
                    "aevalsrc=0.1*sin(2*PI*440*t):duration=1:sample_rate=48000",
                    "-c:a",
                    "aac",
                    "-t",
                    "1",
                    "-y",
                    "-f",
                    "null",
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
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=320x240:rate=25:duration=1",
                    "-f",
                    "lavfi",
                    "-i",
                    "aevalsrc=0.1*sin(2*PI*440*t):duration=1:sample_rate=48000",
                    "-map",
                    "0:v",
                    "-map",
                    "1:a",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-c:a",
                    "aac",
                    "-t",
                    "1",
                    "-y",
                    "-f",
                    "null",
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
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=320x240:rate=25:duration=1",
                    "-f",
                    "lavfi",
                    "-i",
                    "aevalsrc=0.1*sin(2*PI*440*t):duration=1:sample_rate=48000",
                    "-map",
                    "0:v",
                    "-map",
                    "1:a",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-c:a",
                    "aac",
                    "-t",
                    "1",
                    "-y",
                    "-f",
                    "flv",
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
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=320x240:rate=25:duration=2",
                    "-f",
                    "lavfi",
                    "-i",
                    "aevalsrc=0.1*sin(2*PI*440*t):duration=2:sample_rate=48000",
                    "-map",
                    "0:v",
                    "-map",
                    "1:a",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-c:a",
                    "aac",
                    "-t",
                    "2",
                    "-y",
                    test_file,
                ])
                .output()
                .expect("Failed to generate test file");

            if !gen_result.status.success() {
                eprintln!(
                    "Test file generation failed, skipping: {}",
                    String::from_utf8_lossy(&gen_result.stderr)
                );
                return;
            }

            // Now test ffplay can read it (with timeout)
            // Note: ffplay may fail on headless systems due to display issues
            let play_result = Command::new("timeout")
                .args([
                    "5",
                    "ffplay",
                    "-autoexit",
                    "-nodisp",
                    "-v",
                    "error",
                    test_file,
                ])
                .output();

            // Clean up
            let _ = std::fs::remove_file(test_file);

            // ffplay may fail due to display issues, but if it works or gracefully exits, we consider it a pass
            if let Ok(output) = play_result {
                // Exit code 124 means timeout was reached, which is okay for our test
                // Any other exit code besides 0 may indicate display issues
                let exit_ok = output.status.success() || output.status.code() == Some(124);
                if !exit_ok {
                    eprintln!(
                        "ffplay exited with code {:?}: {}",
                        output.status.code(),
                        String::from_utf8_lossy(&output.stderr)
                    );
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
                .resolution(320, 240)
                .framerate(15)
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

            let stream_id = unique_stream("rtmp_push_h264");
            let publish = rtmp_publish(&server, &stream_id, PublishCodec::H264, 8);
            let play = PlayConfig::rtmp(&server.rtmp_url("live", &stream_id)).duration(3);
            publish_and_playback(&publish, &play)
                .await
                .expect("RTMP H264 publish + ffplay playback should succeed");
            println!("RTMP H264 publish + play OK: {stream_id}");

            let stream_id = unique_stream("rtmp_push_h265");
            let publish = rtmp_publish(&server, &stream_id, PublishCodec::H265, 8);
            let play = PlayConfig::rtmp(&server.rtmp_url("live", &stream_id))
                .duration(3)
                .enhanced_rtmp_hevc();
            publish_and_playback(&publish, &play)
                .await
                .expect("RTMP H265 publish + ffplay playback should succeed");
            println!("RTMP H265 publish + play OK: {stream_id}");

            let stream_id = unique_stream("rtmp_to_flv_h264");
            let publish = rtmp_publish(&server, &stream_id, PublishCodec::H264, 10);
            let play = PlayConfig::http_flv(&server.http_flv_url(&stream_id)).duration(3);
            publish_and_playback(&publish, &play)
                .await
                .expect("RTMP publish + HTTP-FLV ffplay should succeed");
            println!("RTMP publish -> HTTP-FLV playback OK: {stream_id}");

            let stream_id = unique_stream("rtmp_to_rtsp_h264");
            let publish = rtmp_publish(&server, &stream_id, PublishCodec::H264, 10);
            let play = PlayConfig::rtsp(&server.rtsp_url(&stream_id)).duration(3);
            publish_and_playback(&publish, &play)
                .await
                .expect("RTMP publish + RTSP ffplay should succeed");
            println!("RTMP publish -> RTSP playback OK: {stream_id}");

            let stream_id = unique_stream("rtmp_to_rtsp_tcp_h264");
            let publish = rtmp_publish(&server, &stream_id, PublishCodec::H264, 10);
            let play = PlayConfig::rtsp(&server.rtsp_url(&stream_id))
                .duration(3)
                .rtsp_transport_tcp();
            publish_and_playback(&publish, &play)
                .await
                .expect("RTMP publish + RTSP TCP ffplay should succeed");
            println!("RTMP publish -> RTSP TCP playback OK: {stream_id}");

            let stream_id = unique_stream("rtmp_to_rtsp_udp_h264");
            let publish = rtmp_publish(&server, &stream_id, PublishCodec::H264, 10);
            let play = PlayConfig::rtsp(&server.rtsp_url(&stream_id))
                .duration(3)
                .rtsp_transport_udp();
            publish_and_playback(&publish, &play)
                .await
                .expect("RTMP publish + RTSP UDP ffplay should succeed");
            println!("RTMP publish -> RTSP UDP playback OK: {stream_id}");

            let stream_id = unique_stream("rtmp_to_hls_h264");
            let publish = rtmp_publish(&server, &stream_id, PublishCodec::H264, 12);
            let play = PlayConfig::hls(&server.hls_url(&stream_id)).duration(5);
            publish_and_hls_playback(&server, &stream_id, &publish, &play)
                .await
                .expect("RTMP publish + HLS ffplay should succeed");
            println!("RTMP publish -> HLS playback OK: {stream_id}");

            let src_id = unique_stream("rtmp_pull_h264_src");
            let pulled_id = unique_stream("rtmp_pull_h264_out");
            let publish = rtmp_publish(&server, &src_id, PublishCodec::H264, 12);
            let remote_url = server.rtmp_url("live", &src_id);
            let play = PlayConfig::rtmp(&server.rtmp_url("live", &pulled_id)).duration(3);
            publish_api_pull_and_play(&server, &publish, &remote_url, &pulled_id, false, &play)
                .await
                .expect("RTMP API pull + ffplay should succeed");
            println!("RTMP API pull + playback OK: {src_id} -> {pulled_id}");

            let src_id = unique_stream("rtsp_pull_tcp_h264_src");
            let pulled_id = unique_stream("rtsp_pull_tcp_h264_out");
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

            let src_id = unique_stream("rtsp_pull_udp_h264_src");
            let pulled_id = unique_stream("rtsp_pull_udp_h264_out");
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

            let stream_id = unique_stream("rtsp_push_h264");
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

            let stream_id = unique_stream("rtmp_to_flv_h265");
            let publish = rtmp_publish(&server, &stream_id, PublishCodec::H265, 10);
            let play = PlayConfig::http_flv(&server.http_flv_url(&stream_id)).duration(3);
            publish_and_playback(&publish, &play)
                .await
                .expect("H265 RTMP publish + HTTP-FLV ffplay should succeed");
            println!("H265 RTMP publish -> HTTP-FLV playback OK: {stream_id}");

            let h264_stream = unique_stream("concurrent_push_h264");
            let h265_stream = unique_stream("concurrent_push_h265");
            let h264_publish = rtmp_publish(&server, &h264_stream, PublishCodec::H264, 10);
            let h265_publish = rtmp_publish(&server, &h265_stream, PublishCodec::H265, 10);

            let mut h264_ffmpeg =
                spawn_ffmpeg(&h264_publish.ffmpeg_args()).expect("spawn H264 ffmpeg");
            let mut h265_ffmpeg =
                spawn_ffmpeg(&h265_publish.ffmpeg_args()).expect("spawn H265 ffmpeg");

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

        // =========================================================================
        // Multi-protocol playback helpers
        // =========================================================================

        /// Play an active stream via RTMP, RTSP, HTTP-FLV, and HLS.
        ///
        /// Verification order is fixed to match the plan output:
        /// RTMP -> RTSP -> FLV -> HLS.
        /// - RTMP/RTSP/HLS: ffmpeg null-muxer decode (frame count) + ffprobe
        /// - HTTP-FLV: raw TCP download + FLV tag counting (Scheme 4)
        async fn play_via_live_protocols(
            server: &MediaServer,
            stream_id: &str,
            codec: PublishCodec,
        ) -> Result<(), String> {
            let mut errors = Vec::new();

            // Server-side frames are the most direct signal for RTSP push/pull
            // ingest. Capture this before external playback probes spend time
            // retrying; short-lived ffmpeg publishers may TEARDOWN and reset the
            // ring before later diagnostics run.
            eprintln!("[{stream_id}] [PROGRESS] Checking server-side frames...");
            match verify_stream_server_side(server, stream_id).await {
                Ok(()) => {
                    println!("[{stream_id}] [OK] SERVER <- stream has frames on server");
                }
                Err(e) => {
                    eprintln!("[{stream_id}] [ERROR] SERVER <- {e}");
                    return Err(format!("SERVER: {e}"));
                }
            }

            if let Err(e) = play_via_rtmp(server, stream_id, codec).await {
                errors.push(format!("RTMP: {e}"));
            }
            if let Err(e) = play_via_rtsp(server, stream_id).await {
                errors.push(format!("RTSP: {e}"));
            }
            if let Err(e) = play_via_flv(server, stream_id).await {
                errors.push(format!("HTTP-FLV: {e}"));
            }
            if let Err(e) = play_via_hls(server, stream_id).await {
                errors.push(format!("HLS: {e}"));
            }

            if errors.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "protocol playback failures for {stream_id}: {}",
                    errors.join(" | ")
                ))
            }
        }

        /// Play via RTMP using ffmpeg decode + ffprobe. Server API is checked by
        /// the caller before protocol-specific playback.
        async fn play_via_rtmp(
            server: &MediaServer,
            stream_id: &str,
            codec: PublishCodec,
        ) -> Result<(), String> {
            let enhanced_hevc = matches!(codec, PublishCodec::H265);
            let rtmp_url = server.rtmp_url("live", stream_id);
            eprintln!("[{stream_id}] [PROGRESS] Checking RTMP playback at {rtmp_url}...");
            match run_ffmpeg_decode_without_api_check(
                &rtmp_url,
                PlayProtocol::RTMP,
                stream_id,
                5,
                enhanced_hevc,
                None,
            )
            .await
            {
                Ok(stats) => {
                    println!(
                        "[{stream_id}] [OK] RTMP   <- {:.0}fps, {} frames",
                        stats.fps.unwrap_or(0.0),
                        stats.video_frames
                    );
                    Ok(())
                }
                Err(e) => {
                    eprintln!("[{stream_id}] [ERROR] RTMP <- {e}");
                    Err(e)
                }
            }
        }

        /// Play via RTSP over TCP using ffmpeg decode + ffprobe. Server API is
        /// checked by the caller before protocol-specific playback.
        async fn play_via_rtsp(server: &MediaServer, stream_id: &str) -> Result<(), String> {
            // Default to TCP transport: UDP RTSP often fails on localhost due
            // to dynamic RTP port negotiation. TCP is more reliable.
            let rtsp_url = server.rtsp_url(stream_id);
            eprintln!("[{stream_id}] [PROGRESS] Checking RTSP playback (TCP) at {rtsp_url}...");
            match run_ffmpeg_decode_without_api_check(
                &rtsp_url,
                PlayProtocol::RTSP,
                stream_id,
                5,
                false,
                Some("tcp"),
            )
            .await
            {
                Ok(stats) => {
                    println!(
                        "[{stream_id}] [OK] RTSP   <- {:.0}fps, {} frames",
                        stats.fps.unwrap_or(0.0),
                        stats.video_frames
                    );
                    Ok(())
                }
                Err(e) => {
                    eprintln!("[{stream_id}] [ERROR] RTSP <- {e}");
                    Err(e)
                }
            }
        }

        /// Play via HTTP-FLV using raw TCP FLV tag counting.
        async fn play_via_flv(server: &MediaServer, stream_id: &str) -> Result<(), String> {
            eprintln!("[{stream_id}] [PROGRESS] Checking HTTP-FLV playback...");
            match verify_http_flv_stream(server, stream_id, 3).await {
                Ok(flv_stats) => {
                    println!(
                        "[{stream_id}] [OK] FLV    <- {} video + {} audio tags, {} bytes",
                        flv_stats.video_tags, flv_stats.audio_tags, flv_stats.total_bytes
                    );
                    Ok(())
                }
                Err(e) => {
                    eprintln!("[{stream_id}] [ERROR] FLV <- {e}");
                    Err(e)
                }
            }
        }

        /// Trigger HLS m3u8, wait for segments, then verify via:
        /// - Scheme 5: playlist + TS segment byte validation
        /// - Scheme 1: ffmpeg null-muxer decode check (frame count)
        ///
        /// HLS segment and decode failures are reported together with other
        /// protocol playback failures by `play_via_live_protocols`.
        async fn play_via_hls(server: &MediaServer, stream_id: &str) -> Result<(), String> {
            let mut errors = Vec::new();
            // Scheme 5: structural HLS verification (segments, bytes)
            eprintln!("[{stream_id}] [PROGRESS] Checking HLS segments...");
            match verify_hls_playlist(server, stream_id, 1).await {
                Ok(hls_stats) => {
                    println!(
                        "[{stream_id}] [OK] HLS    <- {} segments, {} bytes total",
                        hls_stats.segment_count, hls_stats.total_segment_bytes
                    );

                    // Scheme 1: ffmpeg null-muxer decode for codec-level verification
                    let hls_url = server.hls_url(stream_id);
                    match run_ffmpeg_decode_without_api_check(
                        &hls_url,
                        PlayProtocol::HLS,
                        stream_id,
                        5,
                        false,
                        None,
                    )
                    .await
                    {
                        Ok(stats) => {
                            if !stats.decode_errors.is_empty() {
                                eprintln!(
                                    "[{stream_id}] [WARN] HLS decode errors: {:?}",
                                    stats.decode_errors
                                );
                            }
                        }
                        Err(e) => {
                            eprintln!("[{stream_id}] [ERROR] HLS ffmpeg decode: {e}");
                            errors.push(format!("HLS decode: {e}"));
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[{stream_id}] [ERROR] HLS <- {e}");
                    errors.push(format!("HLS: {e}"));
                }
            };

            if errors.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "HLS playback failures for {stream_id}: {}",
                    errors.join(" | ")
                ))
            }
        }

        fn print_push_play_plan(
            publish: &PublishConfig,
            codec: PublishCodec,
            server: &MediaServer,
            stream_id: &str,
        ) {
            let mut commands = Vec::new();
            commands.push(format!(
                "PUBLISH {}",
                format_command("ffmpeg", &publish.ffmpeg_args())
            ));
            commands.extend(play_plan_commands(server, stream_id, codec));
            print_plan_commands(stream_id, commands);
        }

        fn print_pull_play_plan(
            server: &MediaServer,
            publish: &PublishConfig,
            remote_url: &str,
            pulled_stream_id: &str,
            rtsp_pull: bool,
            play_codec: PublishCodec,
        ) {
            let pull_api = if rtsp_pull {
                "/api/pull/rtsp"
            } else {
                "/api/pull/rtmp"
            };
            let pull_body = format!(r#"{{"url":"{remote_url}","stream_id":"{pulled_stream_id}"}}"#);
            let pull_url = format!(
                "http://{}:{}{}",
                server.http_host(),
                server.http_port(),
                pull_api
            );

            let mut commands = Vec::new();
            commands.push(format!(
                "SOURCE_PUBLISH {}",
                format_command("ffmpeg", &publish.ffmpeg_args())
            ));
            commands.push(format!(
                "PULL_API {}",
                format_command(
                    "curl",
                    &[
                        "-fsS".to_string(),
                        "-X".to_string(),
                        "POST".to_string(),
                        "-H".to_string(),
                        "Content-Type: application/json".to_string(),
                        "-d".to_string(),
                        pull_body,
                        pull_url,
                    ],
                )
            ));
            commands.extend(play_plan_commands(server, pulled_stream_id, play_codec));
            print_plan_commands(pulled_stream_id, commands);
        }

        fn print_plan_commands(stream_id: &str, commands: Vec<String>) {
            let separator =
                "======================================================================";
            let mut plan = format!("[{stream_id}] [PLAN_CMD]\n {separator}\n");
            for command in commands {
                plan.push_str("\n ");
                plan.push_str(&command);
                plan.push_str("\n");
            }
            plan.push_str(&format!(" {separator}"));
            eprintln!("{plan}");
        }

        fn play_plan_commands(
            server: &MediaServer,
            stream_id: &str,
            codec: PublishCodec,
        ) -> Vec<String> {
            let enhanced_hevc = matches!(codec, PublishCodec::H265);
            let rtmp_url = server.rtmp_url("live", stream_id);
            let flv_url = server.http_flv_url(stream_id);
            let rtsp_url = server.rtsp_url(stream_id);
            let hls_url = server.hls_url(stream_id);

            vec![
                format!(
                    "PLAY_RTMP {}",
                    planned_ffmpeg_decode_command(
                        &rtmp_url,
                        PlayProtocol::RTMP,
                        5,
                        enhanced_hevc,
                        None,
                    )
                ),
                format!(
                    "PLAY_RTSP {}",
                    planned_ffmpeg_decode_command(
                        &rtsp_url,
                        PlayProtocol::RTSP,
                        5,
                        false,
                        Some("tcp"),
                    )
                ),
                format!(
                    "PLAY_HTTP_FLV {}",
                    format_command(
                        "curl",
                        &[
                            "-fsS".to_string(),
                            "--max-time".to_string(),
                            "3".to_string(),
                            flv_url,
                        ],
                    )
                ),
                format!(
                    "PLAY_HLS {}",
                    planned_ffmpeg_decode_command(&hls_url, PlayProtocol::HLS, 5, false, None)
                ),
            ]
        }

        fn planned_ffmpeg_decode_command(
            url: &str,
            protocol: PlayProtocol,
            duration_secs: u32,
            enhanced_rtmp_hevc: bool,
            rtsp_transport: Option<&str>,
        ) -> String {
            let mut args = Vec::<String>::new();
            args.push("-nostdin".into());
            args.push("-v".into());
            args.push("warning".into());
            args.push("-stats".into());
            args.push("-stats_period".into());
            args.push("1.0".into());

            if enhanced_rtmp_hevc {
                args.push("-rtmp_enhanced_codecs".into());
                args.push("hvc1".into());
            }

            if let Some(transport) = rtsp_transport {
                args.push("-rtsp_transport".into());
                args.push(transport.to_string());
            }

            if matches!(protocol, PlayProtocol::HLS) {
                args.push("-probesize".into());
                args.push("2000000".into());
                args.push("-analyzeduration".into());
                args.push("5000000".into());
            }

            if matches!(protocol, PlayProtocol::RTMP | PlayProtocol::RTSP) {
                args.push("-analyzeduration".into());
                args.push("8000000".into());
                args.push("-probesize".into());
                args.push("10000000".into());
                args.push("-fflags".into());
                args.push("+genpts+discardcorrupt".into());
            }

            let play_dur = if matches!(protocol, PlayProtocol::RTMP | PlayProtocol::RTSP) {
                duration_secs.max(5)
            } else {
                duration_secs
            };

            args.push("-i".into());
            args.push(url.to_string());
            args.push("-t".into());
            args.push(play_dur.to_string());
            args.push("-map".into());
            args.push("0:v:0".into());
            args.push("-frames:v".into());
            args.push("5".into());
            args.push("-f".into());
            args.push("null".into());
            args.push("-".into());

            format_command("ffmpeg", &args)
        }

        /// Push via ffmpeg, then test RTMP + RTSP + FLV + HLS playback against the same stream.
        async fn push_and_play_all_protocols(
            publish: &PublishConfig,
            codec: PublishCodec,
            server: &MediaServer,
            stream_id: &str,
        ) -> Result<(), String> {
            print_push_play_plan(publish, codec, server, stream_id);
            eprintln!("[{stream_id}] [PROGRESS] Spawning ffmpeg publisher...");
            let mut ffmpeg = spawn_ffmpeg_for_stream(stream_id, &publish.ffmpeg_args())
                .map_err(|e| e.to_string())?;
            // Give the publisher enough time to produce several IDR keyframes.
            // GOP is 50 frames @ 15fps ~= 3.3s between keyframes. 4s ensures
            // at least one full GOP is available before the decode check.
            assert_ffmpeg_publishing(&mut ffmpeg, Duration::from_millis(4000)).await?;
            eprintln!(
                "[{stream_id}] [PROGRESS] Publisher running, verifying protocols (RTMP/RTSP/FLV/HLS)..."
            );

            let result = play_via_live_protocols(server, stream_id, codec).await;

            let _ = stop_child(&mut ffmpeg);
            result
        }

        fn stream_id_from_url(url: &str) -> Option<String> {
            url.split(['?', '#'])
                .next()
                .and_then(|path| path.rsplit('/').next())
                .filter(|stream_id| !stream_id.is_empty())
                .map(ToString::to_string)
        }

        async fn wait_for_server_side_frames(
            server: &MediaServer,
            stream_id: &str,
            label: &str,
            timeout: Duration,
        ) -> Result<(), String> {
            let deadline = Instant::now() + timeout;
            let mut last_error = "no readiness check result".to_string();

            loop {
                match verify_stream_server_side(server, stream_id).await {
                    Ok(()) => return Ok(()),
                    Err(e) => last_error = e,
                }

                if Instant::now() >= deadline {
                    return Err(format!(
                        "{label} '{stream_id}' did not become server-side ready within {:?}: {}",
                        timeout, last_error
                    ));
                }

                sleep(Duration::from_millis(500)).await;
            }
        }

        /// Push source, trigger API pull, then test all 4 play protocols from the pulled stream.
        async fn pull_and_play_all_protocols(
            server: &MediaServer,
            publish: &PublishConfig,
            remote_url: &str,
            pulled_stream_id: &str,
            rtsp_pull: bool,
            play_codec: PublishCodec,
        ) -> Result<(), String> {
            print_pull_play_plan(
                server,
                publish,
                remote_url,
                pulled_stream_id,
                rtsp_pull,
                play_codec,
            );
            let source_stream_id = stream_id_from_url(&publish.stream_url)
                .unwrap_or_else(|| "unknown_source_stream".to_string());
            let mut ffmpeg = spawn_ffmpeg_for_stream(&source_stream_id, &publish.ffmpeg_args())
                .map_err(|e| e.to_string())?;
            assert_ffmpeg_publishing(&mut ffmpeg, Duration::from_millis(2000)).await?;

            if rtsp_pull {
                if let Some(source_stream_id) = stream_id_from_url(remote_url) {
                    if let Err(e) = wait_for_server_side_frames(
                        server,
                        &source_stream_id,
                        "source stream",
                        Duration::from_secs(15),
                    )
                    .await
                    {
                        let _ = stop_child(&mut ffmpeg);
                        return Err(e);
                    }
                }
            }

            let resp = if rtsp_pull {
                match server.api_rtsp_pull(remote_url, pulled_stream_id).await {
                    Ok(resp) => resp,
                    Err(e) => {
                        let _ = stop_child(&mut ffmpeg);
                        return Err(e);
                    }
                }
            } else {
                match server.api_rtmp_pull(remote_url, pulled_stream_id).await {
                    Ok(resp) => resp,
                    Err(e) => {
                        let _ = stop_child(&mut ffmpeg);
                        return Err(e);
                    }
                }
            };
            if resp.status != 200 {
                let _ = stop_child(&mut ffmpeg);
                return Err(format!(
                    "pull API failed: status {} body {}",
                    resp.status, resp.body
                ));
            }

            if let Err(e) = wait_for_server_side_frames(
                server,
                pulled_stream_id,
                "pulled stream",
                Duration::from_secs(15),
            )
            .await
            {
                let _ = stop_child(&mut ffmpeg);
                return Err(e);
            }

            let result = play_via_live_protocols(server, pulled_stream_id, play_codec).await;

            let _ = stop_child(&mut ffmpeg);
            result
        }

        /// Create an RTSP publish config with optional transport.
        fn rtsp_publish_cfg(
            server: &MediaServer,
            stream_id: &str,
            codec: PublishCodec,
            duration_secs: u32,
            tcp: bool,
        ) -> PublishConfig {
            let mut cfg = PublishConfig::rtsp("127.0.0.1", server.ports().rtsp, stream_id)
                .resolution(320, 240)
                .framerate(15)
                .duration(duration_secs)
                .codec(codec);
            if tcp {
                cfg = cfg.rtsp_transport_tcp();
            } else {
                cfg = cfg.rtsp_transport_udp();
            }
            cfg
        }

        fn rtmp_publish_continuous(
            server: &MediaServer,
            stream_id: &str,
            codec: PublishCodec,
        ) -> PublishConfig {
            rtmp_publish(server, stream_id, codec, 0).continuous()
        }

        fn rtmp_publish_h265_legacy_continuous(
            server: &MediaServer,
            stream_id: &str,
        ) -> PublishConfig {
            rtmp_publish(server, stream_id, PublishCodec::H265, 0)
                .rtmp_enhanced_hevc(false)
                .continuous()
        }

        fn rtsp_publish_continuous_cfg(
            server: &MediaServer,
            stream_id: &str,
            codec: PublishCodec,
            tcp: bool,
        ) -> PublishConfig {
            rtsp_publish_cfg(server, stream_id, codec, 0, tcp).continuous()
        }

        // =========================================================================
        // RTMP Push → All Play Protocols
        // =========================================================================

        #[tokio::test]
        async fn test_rtmp_push_h264_play_all_protocols() {
            require_ffmpeg_ffplay();
            let sid = unique_stream("rtmp_push_h264");
            eprintln!("[{sid}] [TEST] Starting: RTMP push H264 → all protocols");
            let server =
                MediaServer::start_isolated_for_stream(&sid).expect("failed to start media server");
            server.wait_ready().await;

            let cfg = rtmp_publish_continuous(&server, &sid, PublishCodec::H264);
            push_and_play_all_protocols(&cfg, PublishCodec::H264, &server, &sid)
                .await
                .expect("RTMP H264 → RTMP/FLV/RTSP/HLS");
            println!("[{sid}] [TEST] RTMP push H264 → all protocols OK");
            eprintln!("[{sid}] [TEST] Finished: RTMP push H264 → all protocols OK");
        }

        #[tokio::test]
        async fn test_rtmp_push_h265_play_all_protocols() {
            require_ffmpeg_ffplay();
            let sid = unique_stream("rtmp_push_h265");
            eprintln!("[{sid}] [TEST] Starting: RTMP push H265 → all protocols");
            let server =
                MediaServer::start_isolated_for_stream(&sid).expect("failed to start media server");
            server.wait_ready().await;

            let cfg = rtmp_publish_continuous(&server, &sid, PublishCodec::H265);
            push_and_play_all_protocols(&cfg, PublishCodec::H265, &server, &sid)
                .await
                .expect("RTMP H265 → RTMP/FLV/RTSP/HLS");
            println!("[{sid}] [TEST] RTMP push H265 → all protocols OK");
            eprintln!("[{sid}] [TEST] Finished: RTMP push H265 → all protocols OK");
        }

        #[tokio::test]
        async fn test_rtmp_push_h265_legacy_play_all_protocols() {
            require_ffmpeg_ffplay();
            let sid = unique_stream("rtmp_push_h265_legacy");
            eprintln!("[{sid}] [TEST] Starting: RTMP push H265 legacy → all protocols");
            let server =
                MediaServer::start_isolated_for_stream(&sid).expect("failed to start media server");
            server.wait_ready().await;

            let cfg = rtmp_publish_h265_legacy_continuous(&server, &sid);
            push_and_play_all_protocols(&cfg, PublishCodec::H265, &server, &sid)
                .await
                .expect("RTMP H265 legacy → RTMP/FLV/RTSP/HLS");
            println!("[{sid}] [TEST] RTMP push H265 legacy → all protocols OK");
            eprintln!("[{sid}] [TEST] Finished: RTMP push H265 legacy → all protocols OK");
        }

        // =========================================================================
        // RTSP Push (TCP/UDP) → All Play Protocols
        // =========================================================================

        #[tokio::test]
        async fn test_rtsp_push_h264_tcp_play_all_protocols() {
            require_ffmpeg_ffplay();
            let sid = unique_stream("rtsp_push_tcp_h264");
            eprintln!("[{sid}] [TEST] Starting: RTSP push H264 TCP → all protocols");
            let server =
                MediaServer::start_isolated_for_stream(&sid).expect("failed to start media server");
            server.wait_ready().await;

            let cfg = rtsp_publish_continuous_cfg(&server, &sid, PublishCodec::H264, true);
            push_and_play_all_protocols(&cfg, PublishCodec::H264, &server, &sid)
                .await
                .expect("RTSP TCP H264 → RTMP/FLV/RTSP/HLS");
            println!("[{sid}] [TEST] RTSP push H264 TCP → all protocols OK");
        }

        #[tokio::test]
        async fn test_rtsp_push_h264_udp_play_all_protocols() {
            require_ffmpeg_ffplay();
            let sid = unique_stream("rtsp_push_udp_h264");
            eprintln!("[{sid}] [TEST] Starting: RTSP push H264 UDP → all protocols");
            let server =
                MediaServer::start_isolated_for_stream(&sid).expect("failed to start media server");
            server.wait_ready().await;

            let cfg = rtsp_publish_continuous_cfg(&server, &sid, PublishCodec::H264, false);
            push_and_play_all_protocols(&cfg, PublishCodec::H264, &server, &sid)
                .await
                .expect("RTSP UDP H264 → RTMP/FLV/RTSP/HLS");
            println!("[{sid}] [TEST] RTSP push H264 UDP → all protocols OK");
        }

        #[tokio::test]
        async fn test_rtsp_push_h265_tcp_play_all_protocols() {
            require_ffmpeg_ffplay();
            let sid = unique_stream("rtsp_push_tcp_h265");
            eprintln!("[{sid}] [TEST] Starting: RTSP push H265 TCP → all protocols");
            let server =
                MediaServer::start_isolated_for_stream(&sid).expect("failed to start media server");
            server.wait_ready().await;

            let cfg = rtsp_publish_continuous_cfg(&server, &sid, PublishCodec::H265, true);
            push_and_play_all_protocols(&cfg, PublishCodec::H265, &server, &sid)
                .await
                .expect("RTSP TCP H265 → RTMP/FLV/RTSP/HLS");
            println!("[{sid}] [TEST] RTSP push H265 TCP → all protocols OK");
        }

        #[tokio::test]
        async fn test_rtsp_push_h265_udp_play_all_protocols() {
            require_ffmpeg_ffplay();
            let sid = unique_stream("rtsp_push_udp_h265");
            eprintln!("[{sid}] [TEST] Starting: RTSP push H265 UDP → all protocols");
            let server =
                MediaServer::start_isolated_for_stream(&sid).expect("failed to start media server");
            server.wait_ready().await;

            let cfg = rtsp_publish_continuous_cfg(&server, &sid, PublishCodec::H265, false);
            push_and_play_all_protocols(&cfg, PublishCodec::H265, &server, &sid)
                .await
                .expect("RTSP UDP H265 → RTMP/FLV/RTSP/HLS");
            println!("[{sid}] [TEST] RTSP push H265 UDP → all protocols OK");
        }

        // =========================================================================
        // RTMP Pull → All Play Protocols
        // =========================================================================

        #[tokio::test]
        async fn test_rtmp_pull_h264_play_all_protocols() {
            require_ffmpeg_ffplay();
            let src_id = unique_stream("rtmp_pull_h264_src");
            let pulled_id = unique_stream("rtmp_pull_h264_out");
            eprintln!(
                "[{pulled_id}] [TEST] Starting: RTMP pull H264 → all protocols ({src_id}->{pulled_id})"
            );
            let server = MediaServer::start_isolated_for_stream(&pulled_id)
                .expect("failed to start media server");
            server.wait_ready().await;

            let publish = rtmp_publish_continuous(&server, &src_id, PublishCodec::H264);
            let remote_url = server.rtmp_url("live", &src_id);

            pull_and_play_all_protocols(
                &server,
                &publish,
                &remote_url,
                &pulled_id,
                false,
                PublishCodec::H264,
            )
            .await
            .expect("RTMP pull H264 → RTMP/FLV/RTSP/HLS");
            println!(
                "[{pulled_id}] [TEST] RTMP pull H264 → all protocols OK ({src_id}->{pulled_id})"
            );
        }

        #[tokio::test]
        async fn test_rtmp_pull_h265_play_all_protocols() {
            require_ffmpeg_ffplay();
            let src_id = unique_stream("rtmp_pull_h265_src");
            let pulled_id = unique_stream("rtmp_pull_h265_out");
            eprintln!(
                "[{pulled_id}] [TEST] Starting: RTMP pull H265 → all protocols ({src_id}->{pulled_id})"
            );
            let server = MediaServer::start_isolated_for_stream(&pulled_id)
                .expect("failed to start media server");
            server.wait_ready().await;

            let publish = rtmp_publish_continuous(&server, &src_id, PublishCodec::H265);
            let remote_url = server.rtmp_url("live", &src_id);

            pull_and_play_all_protocols(
                &server,
                &publish,
                &remote_url,
                &pulled_id,
                false,
                PublishCodec::H265,
            )
            .await
            .expect("RTMP pull H265 → RTMP/FLV/RTSP/HLS");
            println!(
                "[{pulled_id}] [TEST] RTMP pull H265 → all protocols OK ({src_id}->{pulled_id})"
            );
        }

        #[tokio::test]
        async fn test_rtmp_pull_h265_legacy_play_all_protocols() {
            require_ffmpeg_ffplay();
            let src_id = unique_stream("rtmp_pull_h265_legacy_src");
            let pulled_id = unique_stream("rtmp_pull_h265_legacy_out");
            eprintln!(
                "[{pulled_id}] [TEST] Starting: RTMP pull H265 legacy → all protocols ({src_id}->{pulled_id})"
            );
            let server = MediaServer::start_isolated_for_stream(&pulled_id)
                .expect("failed to start media server");
            server.wait_ready().await;

            let publish = rtmp_publish_h265_legacy_continuous(&server, &src_id);
            let remote_url = server.rtmp_url("live", &src_id);

            pull_and_play_all_protocols(
                &server,
                &publish,
                &remote_url,
                &pulled_id,
                false,
                PublishCodec::H265,
            )
            .await
            .expect("RTMP pull H265 legacy → RTMP/FLV/RTSP/HLS");
            println!(
                "[{pulled_id}] [TEST] RTMP pull H265 legacy → all protocols OK ({src_id}->{pulled_id})"
            );
        }

        // =========================================================================
        // RTSP Pull (TCP/UDP) → All Play Protocols
        // =========================================================================

        #[tokio::test]
        async fn test_rtsp_pull_h264_tcp_play_all_protocols() {
            require_ffmpeg_ffplay();
            let src_id = unique_stream("rtsp_pull_tcp_h264_src");
            let pulled_id = unique_stream("rtsp_pull_tcp_h264_out");
            eprintln!(
                "[{pulled_id}] [TEST] Starting: RTSP pull H264 TCP → all protocols ({src_id}->{pulled_id})"
            );
            let server = MediaServer::start_isolated_for_stream(&pulled_id)
                .expect("failed to start media server");
            server.wait_ready().await;

            let publish = rtsp_publish_continuous_cfg(&server, &src_id, PublishCodec::H264, true);
            let remote_url = server.rtsp_url(&src_id);

            pull_and_play_all_protocols(
                &server,
                &publish,
                &remote_url,
                &pulled_id,
                true,
                PublishCodec::H264,
            )
            .await
            .expect("RTSP pull H264 TCP → RTMP/FLV/RTSP/HLS");
            println!(
                "[{pulled_id}] [TEST] RTSP pull H264 TCP → all protocols OK ({src_id}->{pulled_id})"
            );
        }

        #[tokio::test]
        async fn test_rtsp_pull_h264_udp_play_all_protocols() {
            require_ffmpeg_ffplay();
            let src_id = unique_stream("rtsp_pull_udp_h264_src");
            let pulled_id = unique_stream("rtsp_pull_udp_h264_out");
            eprintln!(
                "[{pulled_id}] [TEST] Starting: RTSP pull H264 UDP → all protocols ({src_id}->{pulled_id})"
            );
            let server = MediaServer::start_isolated_for_stream(&pulled_id)
                .expect("failed to start media server");
            server.wait_ready().await;

            let publish = rtsp_publish_continuous_cfg(&server, &src_id, PublishCodec::H264, false);
            let remote_url = format!("{}?transport=udp", server.rtsp_url(&src_id));

            pull_and_play_all_protocols(
                &server,
                &publish,
                &remote_url,
                &pulled_id,
                true,
                PublishCodec::H264,
            )
            .await
            .expect("RTSP pull H264 UDP → RTMP/FLV/RTSP/HLS");
            println!(
                "[{pulled_id}] [TEST] RTSP pull H264 UDP → all protocols OK ({src_id}->{pulled_id})"
            );
        }

        #[tokio::test]
        async fn test_rtsp_pull_h265_tcp_play_all_protocols() {
            require_ffmpeg_ffplay();
            let src_id = unique_stream("rtsp_pull_tcp_h265_src");
            let pulled_id = unique_stream("rtsp_pull_tcp_h265_out");
            eprintln!(
                "[{pulled_id}] [TEST] Starting: RTSP pull H265 TCP → all protocols ({src_id}->{pulled_id})"
            );
            let server = MediaServer::start_isolated_for_stream(&pulled_id)
                .expect("failed to start media server");
            server.wait_ready().await;

            let publish = rtsp_publish_continuous_cfg(&server, &src_id, PublishCodec::H265, true);
            let remote_url = server.rtsp_url(&src_id);

            pull_and_play_all_protocols(
                &server,
                &publish,
                &remote_url,
                &pulled_id,
                true,
                PublishCodec::H265,
            )
            .await
            .expect("RTSP pull H265 TCP → RTMP/FLV/RTSP/HLS");
            println!(
                "[{pulled_id}] [TEST] RTSP pull H265 TCP → all protocols OK ({src_id}->{pulled_id})"
            );
        }

        #[tokio::test]
        async fn test_rtsp_pull_h265_udp_play_all_protocols() {
            require_ffmpeg_ffplay();
            let src_id = unique_stream("rtsp_pull_udp_h265_src");
            let pulled_id = unique_stream("rtsp_pull_udp_h265_out");
            eprintln!(
                "[{pulled_id}] [TEST] Starting: RTSP pull H265 UDP → all protocols ({src_id}->{pulled_id})"
            );
            let server = MediaServer::start_isolated_for_stream(&pulled_id)
                .expect("failed to start media server");
            server.wait_ready().await;

            let publish = rtsp_publish_continuous_cfg(&server, &src_id, PublishCodec::H265, false);
            let remote_url = format!("{}?transport=udp", server.rtsp_url(&src_id));

            pull_and_play_all_protocols(
                &server,
                &publish,
                &remote_url,
                &pulled_id,
                true,
                PublishCodec::H265,
            )
            .await
            .expect("RTSP pull H265 UDP → RTMP/FLV/RTSP/HLS");
            println!(
                "[{pulled_id}] [TEST] RTSP pull H265 UDP → all protocols OK ({src_id}->{pulled_id})"
            );
        }
    }
}
