use anyhow::Result;
use std::sync::Arc;
use tracing::{info, debug, warn, error};
use tracing_subscriber::fmt::SubscriberBuilder;

use crate::core::{StreamManager, Track, CodecType, StreamSource, StreamProtocol};
use crate::rtsp::RtspServer;
use crate::rtsp::RtspSession;

pub async fn run_logging_test() -> Result<()> {
    let subscriber = SubscriberBuilder::new()
        .with_max_level(tracing::Level::DEBUG)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    info!("=== Starting RTSP Logging Test Suite ===");
    
    test_basic_rtsp_flow().await?;
    test_error_scenarios().await?;
    test_concurrent_sessions().await?;
    
    info!("=== RTSP Logging Test Suite Completed ===");
    
    Ok(())
}

async fn test_basic_rtsp_flow() -> Result<()> {
    info!("--- Test: Basic RTSP Flow (OPTIONS -> DESCRIBE -> SETUP -> PLAY -> PAUSE -> PLAY -> TEARDOWN) ---");
    
    let manager = Arc::new(StreamManager::new());
    
    let tracks = vec![
        Track {
            id: 0,
            codec: CodecType::H264,
            payload_type: 96,
            clock_rate: 90000,
        },
        Track {
            id: 1,
            codec: CodecType::AAC,
            payload_type: 97,
            clock_rate: 44100,
        },
    ];
    manager.create_stream("test_stream", StreamSource::Push, StreamProtocol::Unknown, None);
    
    let peer_addr: std::net::SocketAddr = "127.0.0.1:55444".parse().unwrap();
    let mut session = RtspSession::new();
    
    info!("Step 1: Send OPTIONS");
    let options_request = "OPTIONS * RTSP/1.0\r\nCSeq: 1\r\n\r\n";
    let _ = RtspServer::process_rtsp_request(options_request, &manager, &mut session, peer_addr, None, None).await;
    
    info!("Step 2: Send DESCRIBE");
    let describe_request = "DESCRIBE rtsp://localhost:554/test_stream RTSP/1.0\r\nCSeq: 2\r\nAccept: application/sdp\r\n\r\n";
    let _ = RtspServer::process_rtsp_request(describe_request, &manager, &mut session, peer_addr, None, None).await;
    
    info!("Step 3: Send SETUP (video)");
    let setup_video_request = "SETUP rtsp://localhost:554/test_stream/trackID=0 RTSP/1.0\r\nCSeq: 3\r\nTransport: RTP/AVP/TCP;interleaved=0-1\r\n\r\n";
    let _ = RtspServer::process_rtsp_request(setup_video_request, &manager, &mut session, peer_addr, None, None).await;
    
    info!("Step 4: Send SETUP (audio)");
    let setup_audio_request = "SETUP rtsp://localhost:554/test_stream/trackID=1 RTSP/1.0\r\nCSeq: 4\r\nTransport: RTP/AVP/TCP;interleaved=2-3\r\n\r\n";
    let _ = RtspServer::process_rtsp_request(setup_audio_request, &manager, &mut session, peer_addr, None, None).await;
    
    info!("Step 5: Send PLAY");
    let play_request = "PLAY rtsp://localhost:554/test_stream RTSP/1.0\r\nCSeq: 5\r\nSession: test-session\r\nRange: npt=0.000-\r\n\r\n";
    let _ = RtspServer::process_rtsp_request(play_request, &manager, &mut session, peer_addr, None, None).await;
    
    info!("Step 6: Send PAUSE");
    let pause_request = "PAUSE rtsp://localhost:554/test_stream RTSP/1.0\r\nCSeq: 6\r\nSession: test-session\r\n\r\n";
    let _ = RtspServer::process_rtsp_request(pause_request, &manager, &mut session, peer_addr, None, None).await;
    
    info!("Step 7: Send PLAY again");
    let play_request_2 = "PLAY rtsp://localhost:554/test_stream RTSP/1.0\r\nCSeq: 7\r\nSession: test-session\r\n\r\n";
    let _ = RtspServer::process_rtsp_request(play_request_2, &manager, &mut session, peer_addr, None, None).await;
    
    info!("Step 8: Send TEARDOWN");
    let teardown_request = "TEARDOWN rtsp://localhost:554/test_stream RTSP/1.0\r\nCSeq: 8\r\nSession: test-session\r\n\r\n";
    let _ = RtspServer::process_rtsp_request(teardown_request, &manager, &mut session, peer_addr, None, None).await;
    
    Ok(())
}

async fn test_error_scenarios() -> Result<()> {
    info!("--- Test: Error Scenarios ---");
    
    let manager = Arc::new(StreamManager::new());
    let peer_addr: std::net::SocketAddr = "127.0.0.1:55555".parse().unwrap();
    let mut session = RtspSession::new();
    
    info!("Test: Invalid method");
    let invalid_request = "INVALID_METHOD /test RTSP/1.0\r\nCSeq: 1\r\n\r\n";
    let _ = RtspServer::process_rtsp_request(invalid_request, &manager, &mut session, peer_addr, None, None).await;
    
    info!("Test: Empty request");
    let empty_request = "";
    let _ = RtspServer::process_rtsp_request(empty_request, &manager, &mut session, peer_addr, None, None).await;
    
    info!("Test: Invalid first line");
    let bad_line_request = "INVALID LINE WITHOUT SPACES\r\nCSeq: 1\r\n\r\n";
    let _ = RtspServer::process_rtsp_request(bad_line_request, &manager, &mut session, peer_addr, None, None).await;
    
    info!("Test: DESCRIBE non-existent stream");
    let describe_not_found = "DESCRIBE rtsp://localhost:554/nonexistent RTSP/1.0\r\nCSeq: 2\r\n\r\n";
    let _ = RtspServer::process_rtsp_request(describe_not_found, &manager, &mut session, peer_addr, None, None).await;
    
    info!("Test: PLAY without stream");
    let play_no_stream = "PLAY rtsp://localhost:554/nonexistent RTSP/1.0\r\nCSeq: 3\r\n\r\n";
    let _ = RtspServer::process_rtsp_request(play_no_stream, &manager, &mut session, peer_addr, None, None).await;
    
    Ok(())
}

async fn test_concurrent_sessions() -> Result<()> {
    info!("--- Test: Concurrent Sessions ---");
    
    let manager = Arc::new(StreamManager::new());
    
    let tracks = vec![
        Track {
            id: 0,
            codec: CodecType::H264,
            payload_type: 96,
            clock_rate: 90000,
        },
    ];
    manager.create_stream("live", StreamSource::Push, StreamProtocol::Unknown, None);
    
    let peer_addr1: std::net::SocketAddr = "192.168.1.100:12345".parse().unwrap();
    let peer_addr2: std::net::SocketAddr = "192.168.1.101:54321".parse().unwrap();
    
    let mut session1 = RtspSession::new();
    let mut session2 = RtspSession::new();
    
    info!("Session 1: OPTIONS + DESCRIBE");
    let _ = RtspServer::process_rtsp_request("OPTIONS * RTSP/1.0\r\nCSeq: 1\r\n\r\n", &manager, &mut session1, peer_addr1, None, None).await;
    let _ = RtspServer::process_rtsp_request("DESCRIBE rtsp://localhost:554/live RTSP/1.0\r\nCSeq: 2\r\nAccept: application/sdp\r\n\r\n", &manager, &mut session1, peer_addr1, None, None).await;
    
    info!("Session 2: OPTIONS + DESCRIBE");
    let _ = RtspServer::process_rtsp_request("OPTIONS * RTSP/1.0\r\nCSeq: 1\r\n\r\n", &manager, &mut session2, peer_addr2, None, None).await;
    let _ = RtspServer::process_rtsp_request("DESCRIBE rtsp://localhost:554/live RTSP/1.0\r\nCSeq: 2\r\nAccept: application/sdp\r\n\r\n", &manager, &mut session2, peer_addr2, None, None).await;
    
    info!("Session 1: SETUP + PLAY");
    let _ = RtspServer::process_rtsp_request("SETUP rtsp://localhost:554/live/trackID=0 RTSP/1.0\r\nCSeq: 3\r\nTransport: RTP/AVP/TCP;interleaved=0-1\r\n\r\n", &manager, &mut session1, peer_addr1, None, None).await;
    let _ = RtspServer::process_rtsp_request("PLAY rtsp://localhost:554/live RTSP/1.0\r\nCSeq: 4\r\n\r\n", &manager, &mut session1, peer_addr1, None, None).await;
    
    info!("Session 2: SETUP + PLAY");
    let _ = RtspServer::process_rtsp_request("SETUP rtsp://localhost:554/live/trackID=0 RTSP/1.0\r\nCSeq: 3\r\nTransport: RTP/AVP/TCP;interleaved=0-1\r\n\r\n", &manager, &mut session2, peer_addr2, None, None).await;
    let _ = RtspServer::process_rtsp_request("PLAY rtsp://localhost:554/live RTSP/1.0\r\nCSeq: 4\r\n\r\n", &manager, &mut session2, peer_addr2, None, None).await;
    
    info!("Both sessions: TEARDOWN");
    let _ = RtspServer::process_rtsp_request("TEARDOWN rtsp://localhost:554/live RTSP/1.0\r\nCSeq: 5\r\n\r\n", &manager, &mut session1, peer_addr1, None, None).await;
    let _ = RtspServer::process_rtsp_request("TEARDOWN rtsp://localhost:554/live RTSP/1.0\r\nCSeq: 5\r\n\r\n", &manager, &mut session2, peer_addr2, None, None).await;
    
    Ok(())
}
