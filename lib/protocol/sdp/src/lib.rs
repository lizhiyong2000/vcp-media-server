pub mod fmtp;
pub mod rtpmap;

pub mod errors;

#[cfg(test)]
mod sdp_test;


use vcp_media_common::{Marshal, Unmarshal};
use rtpmap::RtpMap;
use std::collections::HashMap;
use std::time::Duration;
use crate::errors::SdpError;
use crate::errors::SdpError::SessionOriginError;
use self::fmtp::Fmtp;


//


#[derive(Default, Debug, Clone)]
pub struct SessionOrigin {
    username:       String,
    session_id:      u64,
    session_version: u64,
    network_type:     String,
    address_type:     String,
    address:  String,
}


impl Unmarshal<&str, Result<Self, SdpError>> for SessionOrigin {
    //  o=- 946685052188730 1 IN IP4 0.0.0.0
    fn unmarshal(raw_data: &str) -> Result<Self, SdpError> {
        let mut origin = SessionOrigin::default();

        let parameters: Vec<&str> = raw_data.split(' ').collect();

        if parameters.len() <6 {
            return Err(SdpError::from(SessionOriginError))
        }

        if let Some(t) = parameters.first() {
            origin.username = t.to_string();
        }

        if let Some(session_id) = parameters.get(1) {
            if let Ok(session_id) = session_id.parse::<u64>() {
                origin.session_id = session_id;
            }
        }

        if let Some(session_version) = parameters.get(2) {
            if let Ok(session_version) = session_version.parse::<u64>() {
                origin.session_version = session_version;
            }
        }

        if let Some(network_type) = parameters.get(3) {
            origin.network_type = network_type.to_string();
        }

        if let Some(address_type) = parameters.get(4) {
            origin.address_type = address_type.to_string();
        }

        if let Some(address) = parameters.get(5) {
            origin.address = address.to_string();
        }

        Ok(origin)
    }
}

impl Marshal<String> for SessionOrigin {
    fn marshal(&self) -> String {
        format!("{} {} {} {} {} {}", self.username, self.session_id, self.session_version, self.network_type, self.address_type, self.address)
    }
}


#[derive(Default, Debug, Clone)]
pub struct SessionConnection {
    network_type:     String,
    address_type:     String,
    address:  String,
}

impl Unmarshal<&str, Result<Self, SdpError>> for SessionConnection {
    //  c=IN IP4 0.0.0.0
    fn unmarshal(raw_data: &str) -> Result<Self, SdpError> {
        let mut connection = SessionConnection::default();

        let parameters: Vec<&str> = raw_data.split(' ').collect();

        if let Some(network_type) = parameters.first() {
            connection.network_type = network_type.to_string();
        }

        if let Some(address_type) = parameters.get(1) {
            connection.address_type = address_type.to_string();
        }

        if let Some(address) = parameters.get(2) {
            connection.address = address.to_string();
        }

        Ok(connection)
    }
}

impl Marshal<String> for SessionConnection {
    fn marshal(&self) -> String {
        format!("c={} {} {}\r\n", self.network_type, self.address_type, self.address)
    }
}

#[derive(Debug, Clone, Default)]
pub struct SessionBandwidth {
    b_type: String,
    bandwidth: u16,
}

impl Unmarshal<&str, Result<Self, SdpError>> for SessionBandwidth {
    //   b=AS:284\r\n\
    fn unmarshal(raw_data: &str) -> Result<Self, SdpError> {
        let mut sdp_bandwidth = SessionBandwidth::default();

        let parameters: Vec<&str> = raw_data.split(':').collect();
        if let Some(t) = parameters.first() {
            sdp_bandwidth.b_type = t.to_string();
        }

        if let Some(bandwidth) = parameters.get(1) {
            if let Ok(bandwidth) = bandwidth.parse::<u16>() {
                sdp_bandwidth.bandwidth = bandwidth;
            }
        }

        Ok(sdp_bandwidth)
    }
}

impl Marshal<String> for SessionBandwidth {
    fn marshal(&self) -> String {
        format!("b={}:{}\r\n", self.b_type, self.bandwidth)
    }
}



#[derive(Debug, Clone, Default)]
pub struct SessionTimeDescription {
    timing: SessionTiming,
    repeats: Vec<SessionRepeat>,
}

// impl Unmarshal<&str, Option<crate::SessionTimeDescription>> for crate::SessionTimeDescription {
//     //   b=AS:284\r\n\
//     fn unmarshal(raw_data: &str) -> Result<Self, SdpError> {
//         let mut time = crate::SessionTimeDescription::default();
//
//         // let parameters: Vec<&str> = raw_data.split(':').collect();
//         // if let Some(t) = parameters.first() {
//         //     sdp_bandwidth.b_type = t.to_string();
//         // }
//         //
//         // if let Some(bandwidth) = parameters.get(1) {
//         //     if let Ok(bandwidth) = bandwidth.parse::<u16>() {
//         //         sdp_bandwidth.bandwidth = bandwidth;
//         //     }
//         // }
//         //
//         Some(time)
//     }
// }
//
// impl Marshal<String> for crate::SessionTimeDescription {
//     fn marshal(&self) -> String {
//         format!("{}:{}\r\n", self.timing.marshal(), self.repeats)
//     }
// }


#[derive(Debug, Clone, Default)]
pub struct SessionTiming {
    start:u64,
    stop:u64
}


impl Unmarshal<&str, Result<Self, SdpError>> for crate::SessionTiming {
    //   b=AS:284\r\n\
    fn unmarshal(raw_data: &str) -> Result<Self, SdpError> {
        let mut time = crate::SessionTiming::default();

        let parameters: Vec<&str> = raw_data.split(' ').collect();
        if let Some(start) = parameters.first() {
            if let Ok(start) = start.parse::<u64>() {
                time.start = start;
            }
        }

        if let Some(stop) = parameters.get(1) {
            if let Ok(stop) = stop.parse::<u64>() {
                time.stop = stop;
            }
        }

        Ok(time)
    }
}

impl Marshal<String> for crate::SessionTiming {
    fn marshal(&self) -> String {
        format!("{} {}\r\n", self.start, self.stop)
    }
}

#[derive(Debug, Clone, Default)]
pub struct SessionRepeat {
    interval:i64,
    duration:i64,
    offsets:Vec<i64>
}


impl Unmarshal<&str, Result<Self, SdpError>> for crate::SessionRepeat {
    //   b=AS:284\r\n\
    fn unmarshal(raw_data: &str) -> Result<Self, SdpError> {
        let mut repeat = crate::SessionRepeat::default();

        let parameters: Vec<&str> = raw_data.split(' ').collect();
        if let Some(interval) = parameters.first() {
            if let Ok(interval) = interval.parse::<i64>() {
                repeat.interval = interval;
            }
        }

        if let Some(duration) = parameters.get(1) {
            if let Ok(duration) = duration.parse::<i64>() {
                repeat.duration = duration;
            }
        }

        Ok(repeat)
    }
}

impl Marshal<String> for crate::SessionRepeat {
    fn marshal(&self) -> String {
        let offsets = self.offsets
            .iter()
            .map(|num| num.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        format!("{} {} {}\r\n", self.interval, self.duration, offsets)
    }
}

/*
v=0
o=- 946685052188730 1 IN IP4 0.0.0.0
s=RTSP/RTP Server
i=playback/robot=040082d087c335e3bd2b/camera=head/timerang1=1533620879-1533620898
t=0 0
a=tool:vlc 0.9.8a
a=type:broadcast
a=control:*
a=range:npt=0-
m=video 20003 RTP/AVP 97
b=RR:0
a=rtpmap:97 H264/90000
a=fmtp:97 profile-level-id=42C01E;packetization-mode=1;sprop-parameter-sets=Z0LAHtkDxWhAAAADAEAAAAwDxYuSAAAAAQ==,aMuMsgAAAAE=
a=control:track1
m=audio 11704 RTP/AVP 96 97 98 0 8 18 101 99 100 */

#[derive(Default, Debug, Clone)]
pub struct SessionMediaInfo {
    pub media_type: String,
    pub port: usize,
    pub protocol: String,
    pub fmts: Vec<u8>,


    // i=<session description>
    pub media_title: String,
    // c=<nettype> <addrtype> <connection-address>
    pub connection: Option<SessionConnection>,
    // b=<bwtype>:<bandwidth>
    pub bandwidth: Vec<SessionBandwidth>,
    pub encryption_key: String,
    // pub bandwidth: Option<SessionBandwidth>,
    pub attributes: HashMap<String, String>,

    pub rtpmap: Option<RtpMap>,
    pub fmtp: Vec<fmtp::Fmtp>,
}


impl SessionMediaInfo{
    pub fn get_control(&self) -> String{
        return self.get_attribute("control");
    }

    pub fn get_type(&self) -> String{
        return self.media_type.clone();
    }

    pub fn get_id(&self) -> String{
        return self.get_attribute("mid");
    }

    fn get_attribute(&self, key: &str) -> String{
        if let Some(v)  = self.attributes.get(key){
            v.clone()
        }else{
            String::from("")
        }

    }
}

// impl std::fmt::Debug for dyn TMsgConverter {
//     fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
//         write!(fmt, "S2 {{ member: {:?} }}", self.member)
//     }
// }

// impl Default for SdpMediaInfo {
//     fn default() -> Self {
//         Self {
//             fmtp: Box::new(fmtp::UnknownFmtpSdp::default()),
//             ..Default::default()
//         }
//     }
// }



impl Unmarshal<&str, Result<Self, SdpError>> for SessionMediaInfo {
    //m=audio 11704 RTP/AVP 96 97 98 0 8 18 101 99 100 */
    //m=video 20003 RTP/AVP 97
    fn unmarshal(raw_data: &str) -> Result<Self, SdpError> {
        let mut sdp_media = SessionMediaInfo::default();
        let parameters: Vec<&str> = raw_data.split(' ').collect();

        if let Some(para_0) = parameters.first() {
            sdp_media.media_type = para_0.to_string();
        }

        if let Some(para_1) = parameters.get(1) {
            if let Ok(port) = para_1.parse::<usize>() {
                sdp_media.port = port;
            }
        }

        if let Some(para_2) = parameters.get(2) {
            sdp_media.protocol = para_2.to_string();
        }

        let mut cur_param_idx = 3;

        while let Some(fmt_str) = parameters.get(cur_param_idx) {
            if let Ok(fmt) = fmt_str.parse::<u8>() {
                sdp_media.fmts.push(fmt);
            }
            cur_param_idx += 1;
        }

        Ok(sdp_media)
    }
}

// m=video 0 RTP/AVP 96\r\n\
// b=AS:284\r\n\
// a=rtpmap:96 H264/90000\r\n\
// a=fmtp:96 packetization-mode=1; sprop-parameter-sets=Z2QAHqzZQKAv+XARAAADAAEAAAMAMg8WLZY=,aOvjyyLA; profile-level-id=64001E\r\n\
// a=control:streamid=0\r\n\
// m=audio 0 RTP/AVP 97\r\n\
// b=AS:128\r\n\
// a=rtpmap:97 MPEG4-GENERIC/48000/2\r\n\
// a=fmtp:97 profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3; config=119056E500\r\n\
// a=control:streamid=1\r\n"

impl Marshal<String> for SessionMediaInfo {
    fn marshal(&self) -> String {
        let fmts_str = self
            .fmts
            .iter()
            .map(|b| b.to_string())
            .collect::<Vec<String>>()
            .join(" ");

        // let bandwidth = if let Some(bandwidth) = &self.bandwidth {
        //     format!("b={}", bandwidth.marshal())
        // } else {
        //     String::from("")
        // };


        let mut media_info = format!(
            "m={} {} {} {}\r\n",
            self.media_type,
            self.port,
            self.protocol,
            fmts_str,
        );

        if let Some(connection) = &self.connection{
            media_info = format!("{media_info}{}", connection.marshal());
        }


        if self.media_title.len() > 0{
            media_info = format!("{media_info}i={}\r\n", self.media_title);
        }

        if self.encryption_key.len() > 0{
            media_info = format!("{media_info}k={}\r\n", self.encryption_key);
        }

        if self.bandwidth.len() > 0 {
            let mut bw_str = "".to_string();

            for bw in &self.bandwidth {
                media_info = format!("{media_info}{}", bw.marshal());
            }
        }

        // if let Some(rtpmap) = &self.rtpmap {
        //     sdp_media_info = format!("{}a=rtpmap:{}", sdp_media_info, rtpmap.marshal());
        // }
        //
        // if let Some(fmtp) = &self.fmtp {
        //     for f in fmtp{
        //         sdp_media_info = format!("{}a=fmtp:{}", sdp_media_info, f.marshal());
        //     }
        //
        // }

        for (k, v) in &self.attributes {
            if v.len() > 0{
                media_info = format!("{media_info}a={k}:{v}\r\n");
            }else{
                media_info = format!("{media_info}a={k}\r\n");
            }

        }

        media_info
    }
}



///
/// https://tools.ietf.org/html/rfc4566
///
#[derive(Default, Debug, Clone)]
pub struct SessionDescription {
    raw_string: String,

    // v=0
    version: u16,
    // o=<username> <sess-id> <sess-version> <nettype> <addrtype> <unicast-address>
    origin: SessionOrigin,
    // s=<session name>
    session_name: String,
    // i=<session description>
    session_information: String,
    // u=<uri>
    uri: String,
    // e=<email-address>
    email: String,
    // p=<phone-number>
    phone_number: String,
    // c=<nettype> <addrtype> <connection-address>
    connection: Option<SessionConnection>,

    // b=<bwtype>:<bandwidth>
    bandwidth: Vec<SessionBandwidth>,

    time_descriptions: Vec<SessionTimeDescription>,
    //timing: String,

    // z=<adjustment time> <offset> <adjustment time> <offset> ...

    encryption_key: String,
    // k=<method>
    // k=<method>:<encryption key>

    // a=<attribute>
    // a=<attribute>:<value>
    pub attributes: HashMap<String, String>,

    // https://tools.ietf.org/html/rfc4566#section-5.14
    pub medias: Vec<SessionMediaInfo>,
}

impl Unmarshal<&str, Result<Self, SdpError>>  for SessionDescription {
    fn unmarshal(raw_data: &str) -> Result<Self, SdpError> {
        let mut sdp = SessionDescription {
            raw_string: raw_data.to_string(),
            ..Default::default()
        };

        let lines: Vec<&str> = raw_data.split(|c| c == '\r' || c == '\n').collect();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let kv: Vec<&str> = line.trim().splitn(2, '=').collect();
            if kv.len() < 2 {
                log::error!("Sdp current line : {} parse error!", line);
                continue;
            }

            match kv[0] {
                //m=audio 11704 RTP/AVP 96 97 98 0 8 18 101 99 100 */
                //m=video 20003 RTP/AVP 97

                // v=0\r\n\
                // o=- 0 0 IN IP4 127.0.0.1\r\n\
                // s=No Name\r\n\
                // c=IN IP4 127.0.0.1\r\n\
                // t=0 0\r\n\

                // m=video 0 RTP/AVP 96\r\n\
                // b=AS:284\r\n\
                // a=rtpmap:96 H264/90000\r\n\
                // a=fmtp:96 packetization-mode=1; sprop-parameter-sets=Z2QAHqzZQKAv+XARAAADAAEAAAMAMg8WLZY=,aOvjyyLA; profile-level-id=64001E\r\n\
                // a=control:streamid=0\r\n\
                // m=audio 0 RTP/AVP 97\r\n\
                // b=AS:128\r\n\
                // a=rtpmap:97 MPEG4-GENERIC/48000/2\r\n\
                // a=fmtp:97 profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3; config=119056E500\r\n\
                // a=control:streamid=1\r\n";
                "v" => {
                    if let Ok(version) = kv[1].parse::<u16>() {
                        sdp.version = version;
                    }
                }
                "o" => {
                    // sdp.origin = kv[1].to_string();
                    if let origin = SessionOrigin::unmarshal(kv[1])?{
                        sdp.origin = origin;
                    }
                }
                "s" => {
                    sdp.session_name =  kv[1].to_string();
                }

                "c" => {
                    if let connection= SessionConnection::unmarshal(kv[1])?{
                        if let Some(cur_media) = sdp.medias.last_mut() {
                            cur_media.connection = Some(connection);
                        } else {
                            sdp.connection = Some(connection);
                        }
                    }
                }


                "b" => {
                    if let bandwith = SessionBandwidth::unmarshal(kv[1])?{
                        if let Some(cur_media) = sdp.medias.last_mut() {
                            cur_media.bandwidth.push(bandwith);
                        } else {
                            sdp.bandwidth.push(bandwith);
                        }
                    }

                }

                "k" => {
                    if let Some(cur_media) = sdp.medias.last_mut() {
                        cur_media.encryption_key = kv[1].to_string();
                    } else {
                        sdp.encryption_key = kv[1].to_string();
                    }

                }

                "t" => {
                    // sdp.timing = kv[1].to_string();
                    if let timing = SessionTiming::unmarshal(kv[1])? {
                        sdp.time_descriptions.push(SessionTimeDescription{
                            timing,
                            repeats: vec![],
                        })
                    }
                }

                "r" => {
                    // sdp.timing = kv[1].to_string();
                    if let repeat = SessionRepeat::unmarshal(kv[1])? {
                        if let Some(cur_timing) = sdp.time_descriptions.last_mut() {
                            cur_timing.repeats.push(repeat);
                        } else {
                            continue;
                        }
                    }
                }

                "m" => {
                    if let sdp_media = SessionMediaInfo::unmarshal(kv[1])?{
                        sdp.medias.push(sdp_media);
                    }
                }
                // a=rtpmap:96 H264/90000\r\n\
                // a=fmtp:96 packetization-mode=1; sprop-parameter-sets=Z2QAHqzZQKAv+XARAAADAAEAAAMAMg8WLZY=,aOvjyyLA; profile-level-id=64001E\r\n\
                // a=control:streamid=0\r\n\
                "a" => {
                    let attribute: Vec<&str> = kv[1].splitn(2, ':').collect();

                    let attr_name = attribute[0];
                    let attr_value = if let Some(val) = attribute.get(1) {
                        val
                    } else {
                        ""
                    };

                    if let Some(cur_media) = sdp.medias.last_mut() {

                        cur_media
                            .attributes
                            .insert(attr_name.to_string(), attr_value.to_string());

                        if attribute.len() == 2 {
                            match attr_name {
                                "rtpmap" => {
                                    if let Ok(rtpmap) = RtpMap::unmarshal(attr_value) {
                                        cur_media.rtpmap = Some(rtpmap);
                                        continue;
                                    }
                                }
                                "fmtp" => {

                                    if let Some(rtpmap) = &cur_media.rtpmap{

                                        if let Ok(fmtp) = Fmtp::new(rtpmap.encoding_name.as_str(), attr_value){
                                            cur_media.fmtp.push(fmtp);
                                        }

                                    }
                                    // cur_media.fmtp =
                                    //     Fmtp::new(&cur_media.rtpmap.unwrap().encoding_name, attr_value);
                                    continue;
                                }
                                _ => {}
                            }
                        }

                    } else {
                        sdp.attributes
                            .insert(attr_name.to_string(), attr_value.to_string());
                    }
                }

                _ => {
                    log::info!("not parsed: {}", line);
                }
            }
        }

        Ok(sdp)
    }
}

// v=0\r\n\n
// o=- 0 0 IN IP4 127.0.0.1\r\n\
// s=No Name\r\n\
// c=IN IP4 127.0.0.1\r\n\
// t=0 0\r\n\
// a=tool:libavformat 58.76.100\r\n\

impl Marshal<String> for SessionDescription {
    fn marshal(&self) -> String {
        let mut sdp_str = format!(
            "v={}\r\no={}\r\ns={}\r\n",
            self.version, self.origin.marshal(), self.session_name
        );

        if let Some(connection) = &self.connection{
            sdp_str = format!("{sdp_str}{}", connection.marshal());
        }


        if self.session_information.len() > 0{
            sdp_str = format!("{sdp_str}i={}\r\n", self.session_information);
        }

        if self.bandwidth.len() > 0 {
            let mut bw_str = "".to_string();

            for bw in &self.bandwidth {
                sdp_str = format!("{sdp_str}{}", bw.marshal());
            }
        }

        if self.encryption_key.len() > 0{
            sdp_str = format!("{sdp_str}k={}\r\n", self.encryption_key);
        }

        for (k, v) in &self.attributes {

            if v.len() > 0{
                sdp_str = format!("{sdp_str}a={k}:{v}\r\n");
            }else{
                sdp_str = format!("{sdp_str}a={k}\r\n");
            }
        }

        for media_info in &self.medias {
            sdp_str = format!("{}{}", sdp_str, media_info.marshal());
        }

        sdp_str
    }
}

#[cfg(test)]
mod tests {

    use vcp_media_common::{Marshal, Unmarshal};

    use super::SessionDescription;

    #[test]
    fn test_parse_sdp() {
        let data2 = "ANNOUNCE rtsp://127.0.0.1:5544/stream RTSP/1.0\r\n\
        Content-Type: application/sdp\r\n\
        CSeq: 2\r\n\
        User-Agent: Lavf58.76.100\r\n\
        Content-Length: 500\r\n\
        \r\n\
        v=0\r\n\
        o=- 0 0 IN IP4 127.0.0.1\r\n\
        s=No Name\r\n\
        c=IN IP4 127.0.0.1\r\n\
        t=0 0\r\n\
        a=tool:libavformat 58.76.100\r\n\
        m=video 0 RTP/AVP 96\r\n\
        b=AS:284\r\n\
        a=rtpmap:96 H264/90000\r\n\
        a=fmtp:96 packetization-mode=1; sprop-parameter-sets=Z2QAHqzZQKAv+XARAAADAAEAAAMAMg8WLZY=,aOvjyyLA; profile-level-id=64001E\r\n\
        a=control:streamid=0\r\n\
        m=audio 0 RTP/AVP 97\r\n\
        b=AS:128\r\n\
        a=rtpmap:97 MPEG4-GENERIC/48000/2\r\n\
        a=fmtp:97 profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3; config=119056E500\r\n\
        a=control:streamid=1\r\n";

        // v=0：SDP版本号，通常为0。
        // o=- 0 0 IN IP4 127.0.0.1：会话的所有者和会话ID，以及会话开始时间和会话结束时间的信息。
        // s=No Name：会话名称或标题。
        // c=IN IP4 127.0.0.1：表示会话数据传输的地址类型(IPv4)和地址(127.0.0.1)。
        // t=0 0：会话时间，包括会话开始时间和结束时间，这里的值都是0，表示会话没有预定义的结束时间。
        // a=tool:libavformat 58.76.100：会话所使用的工具或软件名称和版本号。

        // m=video 0 RTP/AVP 96：媒体类型(video或audio)、媒体格式(RTP/AVP)、媒体格式编号(96)和媒体流的传输地址。
        // b=AS:284：视频流所使用的带宽大小。
        // a=rtpmap:96 H264/90000：视频流所使用的编码方式(H.264)和时钟频率(90000)。
        // a=fmtp:96 packetization-mode=1; sprop-parameter-sets=Z2QAHqzZQKAv+XARAAADAAEAAAMAMg8WLZY=,aOvjyyLA; profile-level-id=64001E：视频流的格式参数，如分片方式、SPS和PPS等。
        // a=control:streamid=0：指定视频流的流ID。

        // m=audio 0 RTP/AVP 97：媒体类型(audio)、媒体格式(RTP/AVP)、媒体格式编号(97)和媒体流的传输地址。
        // b=AS:128：音频流所使用的带宽大小。
        // a=rtpmap:97 MPEG4-GENERIC/48000/2：音频流所使用的编码方式(MPEG4-GENERIC)、采样率(48000Hz)、和通道数(2)。
        // a=fmtp:97 profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3; config=119056E500：音频流的格式参数，如编码方式、采样长度、索引长度等。
        // a=control:streamid=1：指定音频流的流ID。

        if let Ok(sdp) = SessionDescription::unmarshal(data2) {
            println!("sdp : {sdp:?}");

            println!("sdp str : {}", sdp.marshal());
        }
    }
    #[test]
    fn test_str() {
        let fmts: Vec<u8> = vec![5];
        // fmts.push(6);
        let fmts_str = fmts
            .iter()
            .map(|b| b.to_string())
            .collect::<Vec<String>>()
            .join(" ");

        println!("=={fmts_str}==");
    }
}
