pub mod codec;
pub mod channel;
pub mod range;
pub mod track;
pub mod transport;
pub mod errors;

pub use vcp_media_common::http::HttpRequest as RtspRequest;
pub use vcp_media_common::http::HttpResponse as RtspResponse;
pub fn gen_response(status_code: http::StatusCode, rtsp_request: &RtspRequest) -> RtspResponse {
    let reason_phrase = if let Some(reason) = status_code.canonical_reason() {
        reason.to_string()
    } else {
        "".to_string()
    };

    let mut response = RtspResponse {
        version: "RTSP/1.0".to_string(),
        status_code: status_code.as_u16(),
        reason_phrase,
        ..Default::default()
    };

    if let Some(cseq) = rtsp_request.headers.get("CSeq") {
        response
            .headers
            .insert("CSeq".to_string(), cseq.to_string());
    }

    response
}
