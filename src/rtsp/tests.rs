use super::*;
use crate::core::{StreamManager, Stream, Track, CodecType, MediaFrame, StreamSource, StreamProtocol};
use std::sync::Arc;
use tokio::time::{sleep, Duration};

#[tokio::test]
async fn test_rtsp_logging_scenario() {
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
    
    info!("=== Starting RTSP logging test scenario ===");
    
    let options_request = "OPTIONS * RTSP/1.0\r\nCSeq: 1\r\n\r\n";
    info!("Sending OPTIONS request...");
    let _ = RtspServer::process_rtsp_request(options_request, &manager, &mut session, peer_addr, None).await;
    sleep(Duration::from_millis(10)).await;
    
    let describe_request = "DESCRIBE rtsp://localhost:554/test_stream RTSP/1.0\r\nCSeq: 2\r\nAccept: application/sdp\r\n\r\n";
    info!("Sending DESCRIBE request...");
    let _ = RtspServer::process_rtsp_request(describe_request, &manager, &mut session, peer_addr, None).await;
    sleep(Duration::from_millis(10)).await;
    
    let setup_video_request = "SETUP rtsp://localhost:554/test_stream/trackID=0 RTSP/1.0\r\nCSeq: 3\r\nTransport: RTP/AVP/TCP;interleaved=0-1\r\n\r\n";
    info!("Sending SETUP request for video track...");
    let _ = RtspServer::process_rtsp_request(setup_video_request, &manager, &mut session, peer_addr, None).await;
    sleep(Duration::from_millis(10)).await;
    
    let setup_audio_request = "SETUP rtsp://localhost:554/test_stream/trackID=1 RTSP/1.0\r\nCSeq: 4\r\nTransport: RTP/AVP/TCP;interleaved=2-3\r\n\r\n";
    info!("Sending SETUP request for audio track...");
    let _ = RtspServer::process_rtsp_request(setup_audio_request, &manager, &mut session, peer_addr, None).await;
    sleep(Duration::from_millis(10)).await;
    
    let play_request = "PLAY rtsp://localhost:554/test_stream RTSP/1.0\r\nCSeq: 5\r\nSession: test-session\r\nRange: npt=0.000-\r\n\r\n";
    info!("Sending PLAY request...");
    let _ = RtspServer::process_rtsp_request(play_request, &manager, &mut session, peer_addr, None).await;
    sleep(Duration::from_millis(10)).await;
    
    let pause_request = "PAUSE rtsp://localhost:554/test_stream RTSP/1.0\r\nCSeq: 6\r\nSession: test-session\r\n\r\n";
    info!("Sending PAUSE request...");
    let _ = RtspServer::process_rtsp_request(pause_request, &manager, &mut session, peer_addr, None).await;
    sleep(Duration::from_millis(10)).await;
    
    let play_request_2 = "PLAY rtsp://localhost:554/test_stream RTSP/1.0\r\nCSeq: 7\r\nSession: test-session\r\n\r\n";
    info!("Sending PLAY request again...");
    let _ = RtspServer::process_rtsp_request(play_request_2, &manager, &mut session, peer_addr, None).await;
    sleep(Duration::from_millis(10)).await;
    
    let teardown_request = "TEARDOWN rtsp://localhost:554/test_stream RTSP/1.0\r\nCSeq: 8\r\nSession: test-session\r\n\r\n";
    info!("Sending TEARDOWN request...");
    let _ = RtspServer::process_rtsp_request(teardown_request, &manager, &mut session, peer_addr, None).await;
    
    info!("=== RTSP logging test scenario completed ===");
}

#[tokio::test]
async fn test_rtsp_announce_and_record_scenario() {
    let manager = Arc::new(StreamManager::new());
    let peer_addr: std::net::SocketAddr = "127.0.0.1:55555".parse().unwrap();
    let mut session = RtspSession::new();
    
    info!("=== Starting RTSP ANNOUNCE and RECORD logging test ===");
    
    let options_request = "OPTIONS * RTSP/1.0\r\nCSeq: 1\r\n\r\n";
    let _ = RtspServer::process_rtsp_request(options_request, &manager, &mut session, peer_addr, None).await;
    sleep(Duration::from_millis(10)).await;
    
    let sdp_body = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=Test Stream\r\nc=IN IP4 0.0.0.0\r\nt=0 0\r\nm=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\n";
    let announce_request = format!("ANNOUNCE rtsp://localhost:554/record_stream RTSP/1.0\r\nCSeq: 2\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{}", sdp_body.len(), sdp_body);
    info!("Sending ANNOUNCE request...");
    let _ = RtspServer::process_rtsp_request(&announce_request, &manager, &mut session, peer_addr, None).await;
    sleep(Duration::from_millis(10)).await;
    
    let setup_request = "SETUP rtsp://localhost:554/record_stream/trackID=0 RTSP/1.0\r\nCSeq: 3\r\nTransport: RTP/AVP/TCP;interleaved=0-1\r\n\r\n";
    info!("Sending SETUP request...");
    let _ = RtspServer::process_rtsp_request(setup_request, &manager, &mut session, peer_addr, None).await;
    sleep(Duration::from_millis(10)).await;
    
    let record_request = "RECORD rtsp://localhost:554/record_stream RTSP/1.0\r\nCSeq: 4\r\nSession: test-record-session\r\n\r\n";
    info!("Sending RECORD request...");
    let _ = RtspServer::process_rtsp_request(record_request, &manager, &mut session, peer_addr, None).await;
    
    info!("=== RTSP ANNOUNCE and RECORD logging test completed ===");
}

#[tokio::test]
async fn test_rtsp_error_scenario() {
    let manager = Arc::new(StreamManager::new());
    let peer_addr: std::net::SocketAddr = "127.0.0.1:55666".parse().unwrap();
    let mut session = RtspSession::new();
    
    info!("=== Starting RTSP error scenario logging test ===");
    
    let invalid_request = "INVALID_METHOD /test RTSP/1.0\r\nCSeq: 1\r\n\r\n";
    info!("Sending invalid method request...");
    let _ = RtspServer::process_rtsp_request(invalid_request, &manager, &mut session, peer_addr, None).await;
    sleep(Duration::from_millis(10)).await;
    
    let empty_request = "";
    info!("Sending empty request...");
    let _ = RtspServer::process_rtsp_request(empty_request, &manager, &mut session, peer_addr, None).await;
    sleep(Duration::from_millis(10)).await;
    
    let bad_line_request = "INVALID LINE WITHOUT SPACES\r\nCSeq: 1\r\n\r\n";
    info!("Sending request with invalid first line...");
    let _ = RtspServer::process_rtsp_request(bad_line_request, &manager, &mut session, peer_addr, None).await;
    sleep(Duration::from_millis(10)).await;
    
    let describe_not_found = "DESCRIBE rtsp://localhost:554/nonexistent RTSP/1.0\r\nCSeq: 2\r\n\r\n";
    info!("Sending DESCRIBE for non-existent stream...");
    let _ = RtspServer::process_rtsp_request(describe_not_found, &manager, &mut session, peer_addr, None).await;
    sleep(Duration::from_millis(10)).await;
    
    let play_no_stream = "PLAY rtsp://localhost:554/nonexistent RTSP/1.0\r\nCSeq: 3\r\n\r\n";
    info!("Sending PLAY without selecting stream...");
    let _ = RtspServer::process_rtsp_request(play_no_stream, &manager, &mut session, peer_addr, None).await;
    
    info!("=== RTSP error scenario logging test completed ===");
}

#[tokio::test]
async fn test_rtsp_concurrent_sessions() {
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
    
    info!("=== Starting RTSP concurrent sessions test ===");
    
    let peer_addr1: std::net::SocketAddr = "192.168.1.100:12345".parse().unwrap();
    let peer_addr2: std::net::SocketAddr = "192.168.1.101:54321".parse().unwrap();
    
    let mut session1 = RtspSession::new();
    let mut session2 = RtspSession::new();
    
    info!("=== Session 1 (Client A) ===");
    let _ = RtspServer::process_rtsp_request("OPTIONS * RTSP/1.0\r\nCSeq: 1\r\n\r\n", &manager, &mut session1, peer_addr1, None).await;
    sleep(Duration::from_millis(5)).await;
    let _ = RtspServer::process_rtsp_request("DESCRIBE rtsp://localhost:554/live RTSP/1.0\r\nCSeq: 2\r\nAccept: application/sdp\r\n\r\n", &manager, &mut session1, peer_addr1, None).await;
    sleep(Duration::from_millis(5)).await;
    
    info!("=== Session 2 (Client B) ===");
    let _ = RtspServer::process_rtsp_request("OPTIONS * RTSP/1.0\r\nCSeq: 1\r\n\r\n", &manager, &mut session2, peer_addr2, None).await;
    sleep(Duration::from_millis(5)).await;
    let _ = RtspServer::process_rtsp_request("DESCRIBE rtsp://localhost:554/live RTSP/1.0\r\nCSeq: 2\r\nAccept: application/sdp\r\n\r\n", &manager, &mut session2, peer_addr2, None).await;
    sleep(Duration::from_millis(5)).await;
    
    info!("=== Session 1 continues ===");
    let _ = RtspServer::process_rtsp_request("SETUP rtsp://localhost:554/live/trackID=0 RTSP/1.0\r\nCSeq: 3\r\nTransport: RTP/AVP/TCP;interleaved=0-1\r\n\r\n", &manager, &mut session1, peer_addr1, None).await;
    sleep(Duration::from_millis(5)).await;
    let _ = RtspServer::process_rtsp_request("PLAY rtsp://localhost:554/live RTSP/1.0\r\nCSeq: 4\r\n\r\n", &manager, &mut session1, peer_addr1, None).await;
    sleep(Duration::from_millis(5)).await;
    
    info!("=== Session 2 continues ===");
    let _ = RtspServer::process_rtsp_request("SETUP rtsp://localhost:554/live/trackID=0 RTSP/1.0\r\nCSeq: 3\r\nTransport: RTP/AVP/TCP;interleaved=0-1\r\n\r\n", &manager, &mut session2, peer_addr2, None).await;
    sleep(Duration::from_millis(5)).await;
    let _ = RtspServer::process_rtsp_request("PLAY rtsp://localhost:554/live RTSP/1.0\r\nCSeq: 4\r\n\r\n", &manager, &mut session2, peer_addr2, None).await;
    sleep(Duration::from_millis(5)).await;
    
    info!("=== Both sessions teardown ===");
    let _ = RtspServer::process_rtsp_request("TEARDOWN rtsp://localhost:554/live RTSP/1.0\r\nCSeq: 5\r\n\r\n", &manager, &mut session1, peer_addr1, None).await;
    let _ = RtspServer::process_rtsp_request("TEARDOWN rtsp://localhost:554/live RTSP/1.0\r\nCSeq: 5\r\n\r\n", &manager, &mut session2, peer_addr2, None).await;
    
    info!("=== RTSP