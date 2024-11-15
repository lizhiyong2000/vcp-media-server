use {
    super::errors::NetConnectionError, vcp_media_common::bytesio::bytes_reader::BytesReader,
    vcp_media_flv::amf0::amf0_reader::Amf0Reader,
};

#[allow(dead_code)]
pub struct NetConnectionReader {
    reader: BytesReader,
    amf0_reader: Amf0Reader,
}

impl NetConnectionReader {
    #[allow(dead_code)]
    fn onconnect(&mut self) -> Result<(), NetConnectionError> {
        Ok(())
    }
}
