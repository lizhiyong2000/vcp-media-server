use {
    super::{define, errors},
    crate::message::messages::define as message_define,
    byteorder::BigEndian,
    vcp_media_common::bytesio::bytes_reader::BytesReader,
};

pub struct EventMessagesReader {
    pub reader: BytesReader,
}

impl EventMessagesReader {
    pub fn new(reader: BytesReader) -> Self {
        Self { reader }
    }

    pub fn parse_event(
        &mut self,
    ) -> Result<message_define::RtmpMessageData, errors::EventMessagesError> {
        let event_type = self.reader.read_u16::<BigEndian>()?;
        match event_type {
            define::RTMP_EVENT_SET_BUFFER_LENGTH => {
                self.read_set_buffer_length()
            }

            define::RTMP_EVENT_STREAM_BEGIN => {
                self.read_stream_begin()
            }

            define::RTMP_EVENT_STREAM_IS_RECORDED => {
                self.read_stream_is_recorded()
            }

            _ => {
                Err(errors::EventMessagesError::UnknowEventMessageType)
            }
        }
    }
    pub fn read_set_buffer_length(
        &mut self,
    ) -> Result<message_define::RtmpMessageData, errors::EventMessagesError> {
        let stream_id = self.reader.read_u32::<BigEndian>()?;
        let ms = self.reader.read_u32::<BigEndian>()?;

        Ok(message_define::RtmpMessageData::SetBufferLength {
            stream_id,
            buffer_length: ms,
        })
    }

    pub fn read_stream_begin(
        &mut self,
    ) -> Result<message_define::RtmpMessageData, errors::EventMessagesError> {
        let stream_id = self.reader.read_u32::<BigEndian>()?;

        Ok(message_define::RtmpMessageData::StreamBegin {
            stream_id,
        })
    }

    pub fn read_stream_is_recorded(
        &mut self,
    ) -> Result<message_define::RtmpMessageData, errors::EventMessagesError> {
        let stream_id = self.reader.read_u32::<BigEndian>()?;

        Ok(message_define::RtmpMessageData::StreamIsRecorded {
            stream_id,
        })
    }
}
