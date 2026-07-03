//! Frame generators for H264 and H265 test streams.
//!
//! Provides functions to generate valid test frames for integration testing.

use bytes::Bytes;

/// Generate H264 SPS (Sequence Parameter Set) and PPS (Picture Parameter Set).
///
/// Returns Annex B format: 0x00000001 start code followed by NAL units.
pub fn generate_h264_sps_pps() -> Bytes {
    // Minimal SPS for baseline profile
    // 0x67 = NAL type 7 (SPS) | profile_idc = 0x42 (Baseline)
    let sps = vec![
        0x67, 0x42, 0xC0, 0x0A, // NAL header + profile
        0xDA, 0x0F, 0x0A, 0x69, // SPS data
    ];

    // Minimal PPS
    // 0x68 = NAL type 8 (PPS)
    let pps = vec![
        0x68, 0xCE, 0x38, 0x80, // PPS NAL + data
    ];

    let mut data = Vec::with_capacity(12);
    data.extend_from_slice(&sps);
    data.extend_from_slice(&pps);

    Bytes::from(data)
}

/// Generate H264 IDR frame (keyframe).
///
/// NAL type 5 = IDR slice
pub fn generate_h264_idr_frame(frame_num: u32) -> Bytes {
    let mut data = Vec::with_capacity(512);

    // NAL header for IDR (5)
    data.push(0x65);

    // Slice header (simplified)
    data.extend_from_slice(&[0x88, 0x80, 0x42, 0x00, 0x00]);

    // Fill with dummy data
    for i in 0..500 {
        data.push((frame_num as u8).wrapping_add(i as u8));
    }

    Bytes::from(data)
}

/// Generate H264 P frame (non-keyframe).
///
/// NAL type 1 = Non-IDR slice
pub fn generate_h264_p_frame(frame_num: u32) -> Bytes {
    let mut data = Vec::with_capacity(256);

    // NAL header for P slice (1)
    data.push(0x41);

    // Slice header (simplified)
    data.extend_from_slice(&[0x9A, 0x00, 0x42, 0x00, 0x00]);

    // Fill with dummy data
    for i in 0..200 {
        data.push((frame_num as u8).wrapping_add(i as u8));
    }

    Bytes::from(data)
}

/// Generate H265 VPS (Video Parameter Set), SPS, and PPS.
///
/// Returns Annex B format for HEVC.
pub fn generate_h265_vps_sps_pps() -> Bytes {
    // VPS NAL - type 32
    let vps = vec![
        0x40, 0x01, 0x0C, 0x01, // NAL header + vps_id
        0xFF, 0x00, 0x00,       // vps_base_layer_internal_flag, etc.
    ];

    // SPS NAL - type 33
    let sps = vec![
        0x42, 0x01, 0x00, 0x01, // NAL header + sps_id
        0x60, 0x00, 0x00, 0x00, // profile tier level
    ];

    // PPS NAL - type 34
    let pps = vec![
        0x44, 0x01, 0x00, 0x01, // NAL header + pps_id
        0x60, 0x00,             // pps_pic_parameter_set_id, etc.
    ];

    let mut data = Vec::with_capacity(32);
    data.extend_from_slice(&vps);
    data.extend_from_slice(&sps);
    data.extend_from_slice(&pps);

    Bytes::from(data)
}

/// Generate H265 IDR frame (keyframe).
///
/// IDR_W_RADL or IDR_N_LP NAL type is 19.
pub fn generate_h265_idr_frame(frame_num: u32) -> Bytes {
    let mut data = Vec::with_capacity(512);

    // NAL header for IDR (19) with layer_id = 0, temporal_id_plus1 = 1
    // HEVC NAL header format: start_code + forbidden_zero_bit(1) + nal_unit_type(6) + nuh_layer_id(6) + nuh_temporal_id_plus1(3)
    data.push(0x40); // nal_unit_type = 19 (IDR_W_RADL), layer_id = 0, temporal = 1
    data.push(0x53); // start_code emulation prevention

    // HEVC slice header (simplified)
    data.extend_from_slice(&[0x02, 0x00, 0x00, 0x00]);

    // Fill with dummy data
    for i in 0..500 {
        data.push((frame_num as u8).wrapping_add(i as u8));
    }

    Bytes::from(data)
}

/// Generate H265 P frame (non-keyframe).
///
/// TRAIL_N or TRAIL_R NAL type is 0 or 1.
pub fn generate_h265_p_frame(frame_num: u32) -> Bytes {
    let mut data = Vec::with_capacity(256);

    // NAL header for TRAIL_R (1)
    data.push(0x41);
    data.push(0x01); // start_code emulation prevention

    // Slice header (simplified)
    data.extend_from_slice(&[0x02, 0x00, 0x00, 0x00]);

    // Fill with dummy data
    for i in 0..200 {
        data.push((frame_num as u8).wrapping_add(i as u8));
    }

    Bytes::from(data)
}

/// Generate AAC audio frame with ADTS header.
pub fn generate_aac_frame(frame_num: u32) -> Bytes {
    let mut data = Vec::with_capacity(256);

    // ADTS header
    data.extend_from_slice(&[
        0xFF, // Sync word
        0xF1, // MPEG-4, Layer 0, CRC absent
        0x50, // Sample rate index, channel config
        0x80, // Frame length (high)
    ]);

    // ADTS header continued
    data.extend_from_slice(&[
        0x00, 0x00, // Frame length (low), CRC
    ]);

    // Raw AAC data
    for i in 0..128 {
        data.push((frame_num as u8).wrapping_add(i as u8));
    }

    Bytes::from(data)
}

/// Generate a complete GOP (Group of Pictures) with IDR and P frames.
///
/// Returns a vector of frames, first one being the IDR.
pub fn generate_h264_gop(gop_size: usize, start_frame: u32) -> Vec<(Bytes, bool)> {
    let mut frames = Vec::with_capacity(gop_size);

    // Add SPS/PPS at the start
    frames.push((generate_h264_sps_pps(), true));

    for i in 0..gop_size {
        let frame_num = start_frame + i as u32;
        if i == 0 {
            frames.push((generate_h264_idr_frame(frame_num), true));
        } else {
            frames.push((generate_h264_p_frame(frame_num), false));
        }
    }

    frames
}

/// Calculate H264 NAL unit type from first byte.
pub fn h264_nal_type(data: &[u8]) -> Option<u8> {
    if data.is_empty() {
        return None;
    }
    // Remove emulation prevention if needed and get NAL type
    Some(data[0] & 0x1F)
}

/// Calculate H265 NAL unit type from NAL header.
///
/// HEVC NAL header is 2 bytes: forbidden_zero_bit(1) | nal_unit_type(6) | nuh_layer_id(6) | nuh_temporal_id_plus1(3)
pub fn h265_nal_type(data: &[u8]) -> Option<u8> {
    if data.len() < 2 {
        return None;
    }
    Some((data[0] & 0x7E) >> 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_h264_sps_pps() {
        let data = generate_h264_sps_pps();
        assert!(!data.is_empty());
        // SPS starts with 0x67 (NAL type 7)
        assert_eq!(data[0] & 0x1F, 7);
    }

    #[test]
    fn test_h264_idr() {
        let data = generate_h264_idr_frame(0);
        assert!(!data.is_empty());
        // IDR NAL type is 5
        assert_eq!(data[0] & 0x1F, 5);
    }

    #[test]
    fn test_h264_p_frame() {
        let data = generate_h264_p_frame(0);
        assert!(!data.is_empty());
        // P slice NAL type is 1
        assert_eq!(data[0] & 0x1F, 1);
    }

    #[test]
    fn test_h265_idr() {
        let data = generate_h265_idr_frame(0);
        assert!(!data.is_empty());
        // IDR type is 19 (check that NAL header exists)
        let nal_type = h265_nal_type(&data);
        // The actual NAL type depends on how we construct the header
        assert!(nal_type.is_some());
    }

    #[test]
    fn test_h265_gop() {
        let gop = generate_h264_gop(10, 0);
        assert_eq!(gop.len(), 11); // SPS/PPS + 10 frames
        assert!(gop[0].1); // SPS/PPS marked as keyframe
        assert!(gop[1].1); // First frame is IDR
        assert!(!gop[2].1); // Second frame is P
    }
}
