use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

use vcp_media_server::process::transcode::{
    parse_bitrate, parse_resolution, StartTranscodeRequest, StopTranscodeRequest, TranscodeManager,
    TranscodeProfile,
};

mod common;

mod transcode_profile_tests {
    use super::*;

    #[test]
    fn test_transcode_profile_default() {
        let profile = TranscodeProfile::default();

        assert_eq!(profile.video_codec, Some("h264".to_string()));
        assert_eq!(profile.video_bitrate, Some("1000k".to_string()));
        assert_eq!(profile.resolution, None);
        assert_eq!(profile.fps, None);
        assert_eq!(profile.audio_codec, Some("aac".to_string()));
        assert_eq!(profile.audio_bitrate, Some("64k".to_string()));
        assert_eq!(profile.audio_sample_rate, Some(44100));
    }

    #[test]
    fn test_transcode_profile_custom() {
        let profile = TranscodeProfile {
            video_codec: Some("h265".to_string()),
            video_bitrate: Some("2000k".to_string()),
            resolution: Some("1280x720".to_string()),
            fps: Some(30),
            audio_codec: Some("opus".to_string()),
            audio_bitrate: Some("128k".to_string()),
            audio_sample_rate: Some(48000),
        };

        assert_eq!(profile.video_codec, Some("h265".to_string()));
        assert_eq!(profile.video_bitrate, Some("2000k".to_string()));
        assert_eq!(profile.resolution, Some("1280x720".to_string()));
        assert_eq!(profile.fps, Some(30));
        assert_eq!(profile.audio_codec, Some("opus".to_string()));
        assert_eq!(profile.audio_bitrate, Some("128k".to_string()));
        assert_eq!(profile.audio_sample_rate, Some(48000));
    }

    #[test]
    fn test_transcode_profile_serde() {
        let profile = TranscodeProfile {
            video_codec: Some("h264".to_string()),
            video_bitrate: Some("500k".to_string()),
            resolution: Some("640x480".to_string()),
            fps: Some(25),
            audio_codec: None,
            audio_bitrate: None,
            audio_sample_rate: None,
        };

        let json_str = serde_json::to_string(&profile).unwrap();
        let deserialized: TranscodeProfile = serde_json::from_str(&json_str).unwrap();

        assert_eq!(deserialized.video_codec, profile.video_codec);
        assert_eq!(deserialized.video_bitrate, profile.video_bitrate);
        assert_eq!(deserialized.resolution, profile.resolution);
        assert_eq!(deserialized.fps, profile.fps);
        assert_eq!(deserialized.audio_codec, profile.audio_codec);
        assert_eq!(deserialized.audio_bitrate, profile.audio_bitrate);
        assert_eq!(deserialized.audio_sample_rate, profile.audio_sample_rate);
    }
}

mod transcode_manager_tests {
    use super::*;
    use vcp_media_server::core::{StreamManager, TranscodeConfig};

    #[tokio::test]
    async fn test_transcode_manager_start_stop() {
        let stream_manager = Arc::new(StreamManager::new());
        let config = TranscodeConfig {
            enabled: true,
            ffmpeg_path: Some("ffmpeg".to_string()),
            max_workers: Some(4),
        };
        let transcode_manager = TranscodeManager::new(stream_manager, &config, 8081, 1935);

        let req = StartTranscodeRequest {
            source_stream_id: "test_source".to_string(),
            target_stream_id: "test_target".to_string(),
            profile: None,
        };

        let result = transcode_manager.start(req);
        assert!(result.is_ok());
        let session_info = result.unwrap();
        assert_eq!(session_info.source_stream_id, "test_source");
        assert_eq!(session_info.target_stream_id, "test_target");
        assert_eq!(session_info.status, "starting");

        let sessions = transcode_manager.active_sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].source_stream_id, "test_source");

        let stop_req = StopTranscodeRequest {
            source_stream_id: "test_source".to_string(),
            target_stream_id: Some("test_target".to_string()),
        };

        let stop_result = transcode_manager.stop(stop_req);
        assert!(stop_result.is_ok());
        assert_eq!(stop_result.unwrap().source_stream_id, "test_source");

        let sessions = transcode_manager.active_sessions();
        assert_eq!(sessions.len(), 0);
    }

    #[tokio::test]
    async fn test_transcode_manager_disabled() {
        let stream_manager = Arc::new(StreamManager::new());
        let config = TranscodeConfig {
            enabled: false,
            ffmpeg_path: Some("ffmpeg".to_string()),
            max_workers: Some(4),
        };
        let transcode_manager = TranscodeManager::new(stream_manager, &config, 8081, 1935);

        let req = StartTranscodeRequest {
            source_stream_id: "test_source".to_string(),
            target_stream_id: "test_target".to_string(),
            profile: None,
        };

        let result = transcode_manager.start(req);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("transcode is disabled"));
    }

    #[tokio::test]
    async fn test_transcode_manager_max_workers() {
        let stream_manager = Arc::new(StreamManager::new());
        let config = TranscodeConfig {
            enabled: true,
            ffmpeg_path: Some("ffmpeg".to_string()),
            max_workers: Some(1),
        };
        let transcode_manager = TranscodeManager::new(stream_manager, &config, 8081, 1935);

        let req1 = StartTranscodeRequest {
            source_stream_id: "source1".to_string(),
            target_stream_id: "target1".to_string(),
            profile: None,
        };
        let result1 = transcode_manager.start(req1);
        assert!(result1.is_ok());

        let req2 = StartTranscodeRequest {
            source_stream_id: "source2".to_string(),
            target_stream_id: "target2".to_string(),
            profile: None,
        };
        let result2 = transcode_manager.start(req2);
        assert!(result2.is_err());
        assert!(result2.unwrap_err().contains("max transcode workers reached"));

        let stop_req = StopTranscodeRequest {
            source_stream_id: "source1".to_string(),
            target_stream_id: Some("target1".to_string()),
        };
        transcode_manager.stop(stop_req).unwrap();

        let req3 = StartTranscodeRequest {
            source_stream_id: "source3".to_string(),
            target_stream_id: "target3".to_string(),
            profile: None,
        };
        let result3 = transcode_manager.start(req3);
        assert!(result3.is_ok());
    }

    #[tokio::test]
    async fn test_transcode_manager_duplicate_session() {
        let stream_manager = Arc::new(StreamManager::new());
        let config = TranscodeConfig {
            enabled: true,
            ffmpeg_path: Some("ffmpeg".to_string()),
            max_workers: Some(4),
        };
        let transcode_manager = TranscodeManager::new(stream_manager, &config, 8081, 1935);

        let req1 = StartTranscodeRequest {
            source_stream_id: "same_source".to_string(),
            target_stream_id: "same_target".to_string(),
            profile: None,
        };
        let result1 = transcode_manager.start(req1);
        assert!(result1.is_ok());

        let req2 = StartTranscodeRequest {
            source_stream_id: "same_source".to_string(),
            target_stream_id: "same_target".to_string(),
            profile: None,
        };
        let result2 = transcode_manager.start(req2);
        assert!(result2.is_err());
        assert!(result2.unwrap_err().contains("already exists"));

        let stop_req = StopTranscodeRequest {
            source_stream_id: "same_source".to_string(),
            target_stream_id: Some("same_target".to_string()),
        };
        transcode_manager.stop(stop_req).unwrap();
    }

    #[tokio::test]
    async fn test_transcode_manager_stop_nonexistent() {
        let stream_manager = Arc::new(StreamManager::new());
        let config = TranscodeConfig {
            enabled: true,
            ffmpeg_path: Some("ffmpeg".to_string()),
            max_workers: Some(4),
        };
        let transcode_manager = TranscodeManager::new(stream_manager, &config, 8081, 1935);

        let stop_req = StopTranscodeRequest {
            source_stream_id: "nonexistent".to_string(),
            target_stream_id: Some("target".to_string()),
        };

        let result = transcode_manager.stop(stop_req);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[tokio::test]
    async fn test_transcode_manager_stop_by_source_only() {
        let stream_manager = Arc::new(StreamManager::new());
        let config = TranscodeConfig {
            enabled: true,
            ffmpeg_path: Some("ffmpeg".to_string()),
            max_workers: Some(4),
        };
        let transcode_manager = TranscodeManager::new(stream_manager, &config, 8081, 1935);

        let req = StartTranscodeRequest {
            source_stream_id: "source_without_target".to_string(),
            target_stream_id: "target_for_source".to_string(),
            profile: None,
        };
        transcode_manager.start(req).unwrap();

        let stop_req = StopTranscodeRequest {
            source_stream_id: "source_without_target".to_string(),
            target_stream_id: None,
        };

        let result = transcode_manager.stop(stop_req);
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap().target_stream_id,
            "target_for_source"
        );

        let sessions = transcode_manager.active_sessions();
        assert_eq!(sessions.len(), 0);
    }

    #[tokio::test]
    async fn test_transcode_manager_active_sessions_list() {
        let stream_manager = Arc::new(StreamManager::new());
        let config = TranscodeConfig {
            enabled: true,
            ffmpeg_path: Some("ffmpeg".to_string()),
            max_workers: Some(4),
        };
        let transcode_manager = TranscodeManager::new(stream_manager, &config, 8081, 1935);

        assert_eq!(transcode_manager.active_sessions().len(), 0);

        let req1 = StartTranscodeRequest {
            source_stream_id: "stream1".to_string(),
            target_stream_id: "stream1_low".to_string(),
            profile: None,
        };
        transcode_manager.start(req1).unwrap();

        let req2 = StartTranscodeRequest {
            source_stream_id: "stream2".to_string(),
            target_stream_id: "stream2_high".to_string(),
            profile: None,
        };
        transcode_manager.start(req2).unwrap();

        let sessions = transcode_manager.active_sessions();
        assert_eq!(sessions.len(), 2);

        let stop_req1 = StopTranscodeRequest {
            source_stream_id: "stream1".to_string(),
            target_stream_id: Some("stream1_low".to_string()),
        };
        transcode_manager.stop(stop_req1).unwrap();

        let sessions = transcode_manager.active_sessions();
        assert_eq!(sessions.len(), 1);

        let stop_req2 = StopTranscodeRequest {
            source_stream_id: "stream2".to_string(),
            target_stream_id: Some("stream2_high".to_string()),
        };
        transcode_manager.stop(stop_req2).unwrap();

        let sessions = transcode_manager.active_sessions();
        assert_eq!(sessions.len(), 0);
    }
}

mod codec_conversion_tests {
    use super::*;
    use vcp_media_server::core::{StreamManager, TranscodeConfig};

    #[tokio::test]
    async fn test_transcode_h264_to_h265() {
        let stream_manager = Arc::new(StreamManager::new());
        let config = TranscodeConfig {
            enabled: true,
            ffmpeg_path: Some("ffmpeg".to_string()),
            max_workers: Some(4),
        };
        let transcode_manager = TranscodeManager::new(stream_manager, &config, 8081, 1935);

        let profile = TranscodeProfile {
            video_codec: Some("h265".to_string()),
            video_bitrate: Some("1500k".to_string()),
            resolution: Some("1280x720".to_string()),
            fps: Some(30),
            ..Default::default()
        };

        let req = StartTranscodeRequest {
            source_stream_id: "h264_source".to_string(),
            target_stream_id: "h265_target".to_string(),
            profile: Some(profile.clone()),
        };

        let result = transcode_manager.start(req);
        assert!(result.is_ok());
        let session_info = result.unwrap();
        assert_eq!(session_info.profile.video_codec, Some("h265".to_string()));
        assert_eq!(session_info.profile.video_bitrate, Some("1500k".to_string()));
        assert_eq!(session_info.profile.resolution, Some("1280x720".to_string()));

        let stop_req = StopTranscodeRequest {
            source_stream_id: "h264_source".to_string(),
            target_stream_id: Some("h265_target".to_string()),
        };
        transcode_manager.stop(stop_req).unwrap();
    }

    #[tokio::test]
    async fn test_transcode_h265_to_h264() {
        let stream_manager = Arc::new(StreamManager::new());
        let config = TranscodeConfig {
            enabled: true,
            ffmpeg_path: Some("ffmpeg".to_string()),
            max_workers: Some(4),
        };
        let transcode_manager = TranscodeManager::new(stream_manager, &config, 8081, 1935);

        let profile = TranscodeProfile {
            video_codec: Some("h264".to_string()),
            video_bitrate: Some("2000k".to_string()),
            resolution: Some("1920x1080".to_string()),
            fps: Some(25),
            ..Default::default()
        };

        let req = StartTranscodeRequest {
            source_stream_id: "h265_source".to_string(),
            target_stream_id: "h264_target".to_string(),
            profile: Some(profile.clone()),
        };

        let result = transcode_manager.start(req);
        assert!(result.is_ok());
        let session_info = result.unwrap();
        assert_eq!(session_info.profile.video_codec, Some("h264".to_string()));
        assert_eq!(session_info.profile.video_bitrate, Some("2000k".to_string()));
        assert_eq!(session_info.profile.resolution, Some("1920x1080".to_string()));

        let stop_req = StopTranscodeRequest {
            source_stream_id: "h265_source".to_string(),
            target_stream_id: Some("h264_target".to_string()),
        };
        transcode_manager.stop(stop_req).unwrap();
    }

    #[tokio::test]
    async fn test_transcode_same_codec() {
        let stream_manager = Arc::new(StreamManager::new());
        let config = TranscodeConfig {
            enabled: true,
            ffmpeg_path: Some("ffmpeg".to_string()),
            max_workers: Some(4),
        };
        let transcode_manager = TranscodeManager::new(stream_manager, &config, 8081, 1935);

        let profile = TranscodeProfile {
            video_codec: Some("h264".to_string()),
            video_bitrate: Some("500k".to_string()),
            resolution: Some("640x360".to_string()),
            fps: Some(24),
            ..Default::default()
        };

        let req = StartTranscodeRequest {
            source_stream_id: "h264_source".to_string(),
            target_stream_id: "h264_low".to_string(),
            profile: Some(profile),
        };

        let result = transcode_manager.start(req);
        assert!(result.is_ok());

        let stop_req = StopTranscodeRequest {
            source_stream_id: "h264_source".to_string(),
            target_stream_id: Some("h264_low".to_string()),
        };
        transcode_manager.stop(stop_req).unwrap();
    }
}

mod helper_function_tests {
    use super::*;

    #[test]
    fn test_parse_bitrate_kilobits() {
        assert_eq!(parse_bitrate("500k"), Ok(500_000));
        assert_eq!(parse_bitrate("1000k"), Ok(1_000_000));
        assert_eq!(parse_bitrate("2500k"), Ok(2_500_000));
    }

    #[test]
    fn test_parse_bitrate_megabits() {
        assert_eq!(parse_bitrate("1m"), Ok(1_000_000));
        assert_eq!(parse_bitrate("2m"), Ok(2_000_000));
        assert_eq!(parse_bitrate("5m"), Ok(5_000_000));
    }

    #[test]
    fn test_parse_bitrate_raw() {
        assert_eq!(parse_bitrate("500000"), Ok(500_000));
        assert_eq!(parse_bitrate("1000000"), Ok(1_000_000));
    }

    #[test]
    fn test_parse_bitrate_invalid() {
        assert!(parse_bitrate("abc").is_err());
        assert!(parse_bitrate("").is_err());
        assert!(parse_bitrate("500kb").is_err());
    }

    #[test]
    fn test_parse_resolution_valid() {
        assert_eq!(parse_resolution("640x480"), Ok((640, 480)));
        assert_eq!(parse_resolution("1280x720"), Ok((1280, 720)));
        assert_eq!(parse_resolution("1920x1080"), Ok((1920, 1080)));
        assert_eq!(parse_resolution("3840x2160"), Ok((3840, 2160)));
    }

    #[test]
    fn test_parse_resolution_invalid() {
        assert!(parse_resolution("640").is_err());
        assert!(parse_resolution("640x").is_err());
        assert!(parse_resolution("x480").is_err());
        assert!(parse_resolution("invalid").is_err());
        assert!(parse_resolution("1280x720x1080").is_err());
        assert!(parse_resolution("abcxdef").is_err());
    }
}

mod h264_to_h265_integration_tests {
    use super::*;
    use std::time::{Duration, Instant};

    use crate::common::{
        media_server_runner::MediaServer,
        process_manager::{
            assert_ffmpeg_publishing, is_ffmpeg_available, is_ffplay_available, run_ffplay_playback,
            spawn_ffmpeg, stop_child, PlayConfig, PublishCodec, PublishConfig,
        },
    };

    fn unique_stream(prefix: &str) -> String {
        format!(
            "{prefix}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        )
    }

    fn ffmpeg_has_libx265() -> bool {
        std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-encoders"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("libx265"))
            .unwrap_or(false)
    }

    async fn wait_for_video_codec(
        server: &MediaServer,
        stream_id: &str,
        expected: &str,
        timeout: Duration,
    ) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let path = format!("/api/stream/{stream_id}");
        let mut last_body = String::new();
        loop {
            let resp = server.get(&path).await?;
            last_body = resp.body.clone();
            if resp.status == 200 {
                let json: serde_json::Value =
                    serde_json::from_str(&resp.body).map_err(|e| e.to_string())?;
                if let Some(tracks) = json.get("tracks").and_then(|v| v.as_array()) {
                    for track in tracks {
                        if let Some(codec) = track.get("codec").and_then(|v| v.as_str()) {
                            if codec.contains(expected) {
                                return Ok(());
                            }
                        }
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for {expected} on stream {stream_id}; last body: {last_body}"
                ));
            }
            sleep(Duration::from_millis(500)).await;
        }
    }

    #[tokio::test]
    async fn test_h264_publish_transcodes_to_h265_stream() {
        if !is_ffmpeg_available() {
            eprintln!("ffmpeg not available, skipping H264->H265 transcode test");
            return;
        }
        if !ffmpeg_has_libx265() {
            eprintln!("ffmpeg libx265 not available, skipping H264->H265 transcode test");
            return;
        }

        let server = MediaServer::start_isolated().expect(
            "failed to start media server — run `cargo build --bin vcp-media-server` first",
        );
        server.wait_ready().await;

        let source_id = unique_stream("h264_src");
        let target_id = unique_stream("h265_out");

        let publish = PublishConfig::rtmp("127.0.0.1", server.ports().rtmp, "live", &source_id)
            .resolution(640, 480)
            .framerate(25)
            .duration(30)
            .codec(PublishCodec::H264);

        let mut publisher =
            spawn_ffmpeg(&publish.ffmpeg_args()).expect("spawn H264 publisher ffmpeg");
        assert_ffmpeg_publishing(&mut publisher, Duration::from_millis(2500))
            .await
            .expect("H264 publisher should stay alive");

        wait_for_video_codec(&server, &source_id, "H264", Duration::from_secs(15))
            .await
            .expect("source stream should publish H264");

        let start_body = format!(
            r#"{{
                "source_stream_id":"{source_id}",
                "target_stream_id":"{target_id}",
                "profile":{{
                    "video_codec":"h265",
                    "video_bitrate":"1000k",
                    "resolution":"640x480",
                    "fps":25
                }}
            }}"#
        );
        let start_resp = server.api_transcode_start(&start_body).await.expect("transcode start");
        assert_eq!(
            start_resp.status, 200,
            "transcode start failed: {}",
            start_resp.body
        );

        // libx265 首帧 + HTTP-FLV 探针需要几秒
        sleep(Duration::from_secs(3)).await;

        wait_for_video_codec(&server, &target_id, "H265", Duration::from_secs(60))
            .await
            .expect("transcoded target stream should publish H265");

        if is_ffplay_available() {
            let play = PlayConfig::rtmp(&server.rtmp_url("live", &target_id))
                .duration(3)
                .enhanced_rtmp_hevc();
            run_ffplay_playback(&play)
                .await
                .expect("ffplay should decode transcoded H265 RTMP stream");
        }

        let stop_body = format!(
            r#"{{"source_stream_id":"{source_id}","target_stream_id":"{target_id}"}}"#
        );
        let stop_resp = server.api_transcode_stop(&stop_body).await.expect("transcode stop");
        assert_eq!(stop_resp.status, 200, "transcode stop failed: {}", stop_resp.body);

        let _ = stop_child(&mut publisher);
    }
}
