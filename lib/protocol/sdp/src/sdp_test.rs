
use vcp_media_common::{Marshal, Unmarshal};
use super::SessionDescription;
use std::error;

#[test]
fn test_one_format_for_each_media_absolute(){
    let data2 =
"v=0\r\n
o=- 0 0 IN IP4 10.0.0.131\r\n
s=Media Presentation\r\n
i=samsung\r\n
c=IN IP4 0.0.0.0\r\n
b=AS:2632\r\n
t=0 0\r\n
a=control:rtsp://10.0.100.50/profile5/media.smp\r\n
a=range:npt=now-\r\n
m=video 42504 RTP/AVP 97\r\n
b=AS:2560\r\n
a=rtpmap:97 H264/90000\r\n
a=control:rtsp://10.0.100.50/profile5/media.smp/trackID=v\r\n
a=cliprect:0,0,1080,1920\r\n
a=framesize:97 1920-1080\r\n
a=framerate:30.0\r\n
a=fmtp:97 packetization-mode=1;profile-level-id=640028;sprop-parameter-sets=Z2QAKKy0A8ARPyo=,aO4Bniw=\r\n
m=audio 42506 RTP/AVP 0\r\n
b=AS:64\r\n
a=rtpmap:0 PCMU/8000\r\n
a=control:rtsp://10.0.100.50/profile5/media.smp/trackID=a\r\n
a=recvonly\r\n
m=application 42508 RTP/AVP 107\r\n
b=AS:8\r\n";


    if let Some(sdp) = SessionDescription::unmarshal(data2) {
        print!("sdp str : {}", sdp.marshal());
        
    }else {
        assert!(
            false,
            "test_one_format_for_each_media_absolute sdp parse failed."
        );
    }

}


#[test]
fn test_one_format_for_each_media_relative() {
    let data2 =
        "v=0\r\n
o=- 0 0 IN IP4 10.0.0.131\r\n
s=Media Presentation\r\n
i=samsung\r\n
c=IN IP4 0.0.0.0\r\n
b=AS:2632\r\n
t=0 0\r\n
a=range:npt=now-\r\n
m=video 42504 RTP/AVP 97\r\n
b=AS:2560\r\n
a=rtpmap:97 H264/90000\r\n
a=control:trackID=1\r\n
a=cliprect:0,0,1080,1920\r\n
a=framesize:97 1920-1080\r\n
a=framerate:30.0\r\n
a=fmtp:97 packetization-mode=1;profile-level-id=640028;sprop-parameter-sets=Z2QAKKy0A8ARPyo=,aO4Bniw=\r\n
m=audio 42506 RTP/AVP 0\r\n
b=AS:64\r\n
a=rtpmap:0 PCMU/8000\r\n
a=control:trackID=2\r\n
a=recvonly\r\n
m=application 42508 RTP/AVP 107\r\n
b=AS:8\r\n";


    if let Some(sdp) = SessionDescription::unmarshal(data2) {
        // println!("sdp : {sdp:?}");
        print!("sdp str : {}", sdp.marshal());
    }
}

