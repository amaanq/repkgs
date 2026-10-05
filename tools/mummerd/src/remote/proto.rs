//! The REAPI and `ByteStream` messages the remote tier sends, with only the
//! fields it uses. Field numbers follow `remote_execution.proto` and
//! `bytestream.proto`.

use quick_protobuf::{
   BytesReader,
   MessageRead,
   MessageWrite,
   Result,
   Writer,
   WriterBackend,
   sizeofs::{
      sizeof_len,
      sizeof_varint,
   },
};

/// The key a field is written under, its number and wire type.
const fn tag(number: u32, wire: u32) -> u32 {
   (number << 3_u32) | wire
}

const VARINT: u32 = 0;
const LENGTH: u32 = 2;

fn varint_size(value: i64) -> usize {
   if value == 0 {
      0
   } else {
      1 + sizeof_varint(u64::from_ne_bytes(value.to_ne_bytes()))
   }
}

fn bytes_size(length: usize) -> usize {
   if length == 0 {
      0
   } else {
      1 + sizeof_len(length)
   }
}

#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct Digest {
   pub hash:       String,
   pub size_bytes: i64,
}

impl<'bytes> MessageRead<'bytes> for Digest {
   #[expect(
      clippy::renamed_function_params,
      reason = "quick-protobuf names it r, shorter than names here may be"
   )]
   fn from_reader(reader: &mut BytesReader, bytes: &'bytes [u8]) -> Result<Self> {
      let mut message = Self::default();
      while !reader.is_eof() {
         match reader.next_tag(bytes)? {
            10 => reader.read_string(bytes)?.clone_into(&mut message.hash),
            16 => message.size_bytes = reader.read_int64(bytes)?,
            other => reader.read_unknown(bytes, other)?,
         }
      }
      Ok(message)
   }
}

impl MessageWrite for Digest {
   fn get_size(&self) -> usize {
      bytes_size(self.hash.len()) + varint_size(self.size_bytes)
   }

   fn write_message<Backend>(&self, writer: &mut Writer<Backend>) -> Result<()>
   where
      Backend: WriterBackend,
   {
      if !self.hash.is_empty() {
         writer.write_with_tag(tag(1, LENGTH), |inner| inner.write_string(&self.hash))?;
      }
      if self.size_bytes != 0 {
         writer.write_with_tag(tag(2, VARINT), |inner| inner.write_int64(self.size_bytes))?;
      }
      Ok(())
   }
}

#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct OutputFile {
   pub path:   String,
   pub digest: Option<Digest>,
}

impl<'bytes> MessageRead<'bytes> for OutputFile {
   #[expect(
      clippy::renamed_function_params,
      reason = "quick-protobuf names it r, shorter than names here may be"
   )]
   fn from_reader(reader: &mut BytesReader, bytes: &'bytes [u8]) -> Result<Self> {
      let mut message = Self::default();
      while !reader.is_eof() {
         match reader.next_tag(bytes)? {
            10 => reader.read_string(bytes)?.clone_into(&mut message.path),
            18 => message.digest = Some(reader.read_message::<Digest>(bytes)?),
            other => reader.read_unknown(bytes, other)?,
         }
      }
      Ok(message)
   }
}

impl MessageWrite for OutputFile {
   fn get_size(&self) -> usize {
      bytes_size(self.path.len())
         + self
            .digest
            .as_ref()
            .map_or(0, |digest| 1 + sizeof_len(digest.get_size()))
   }

   fn write_message<Backend>(&self, writer: &mut Writer<Backend>) -> Result<()>
   where
      Backend: WriterBackend,
   {
      if !self.path.is_empty() {
         writer.write_with_tag(tag(1, LENGTH), |inner| inner.write_string(&self.path))?;
      }
      if let Some(ref digest) = self.digest {
         writer.write_with_tag(tag(2, LENGTH), |inner| inner.write_message(digest))?;
      }
      Ok(())
   }
}

#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct ActionResult {
   pub output_files: Vec<OutputFile>,
   pub stdout_raw:   Vec<u8>,
}

impl<'bytes> MessageRead<'bytes> for ActionResult {
   #[expect(
      clippy::renamed_function_params,
      reason = "quick-protobuf names it r, shorter than names here may be"
   )]
   fn from_reader(reader: &mut BytesReader, bytes: &'bytes [u8]) -> Result<Self> {
      let mut message = Self::default();
      while !reader.is_eof() {
         match reader.next_tag(bytes)? {
            18 => {
               message
                  .output_files
                  .push(reader.read_message::<OutputFile>(bytes)?);
            },
            42 => {
               reader
                  .read_bytes(bytes)?
                  .clone_into(&mut message.stdout_raw);
            },
            other => reader.read_unknown(bytes, other)?,
         }
      }
      Ok(message)
   }
}

impl MessageWrite for ActionResult {
   fn get_size(&self) -> usize {
      self
         .output_files
         .iter()
         .map(|output| 1 + sizeof_len(output.get_size()))
         .sum::<usize>()
         + bytes_size(self.stdout_raw.len())
   }

   fn write_message<Backend>(&self, writer: &mut Writer<Backend>) -> Result<()>
   where
      Backend: WriterBackend,
   {
      for output in &self.output_files {
         writer.write_with_tag(tag(2, LENGTH), |inner| inner.write_message(output))?;
      }
      if !self.stdout_raw.is_empty() {
         writer.write_with_tag(tag(5, LENGTH), |inner| inner.write_bytes(&self.stdout_raw))?;
      }
      Ok(())
   }
}

#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct GetActionResultRequest {
   pub instance_name: String,
   pub action_digest: Digest,
   pub inline_stdout: bool,
}

impl MessageWrite for GetActionResultRequest {
   fn get_size(&self) -> usize {
      bytes_size(self.instance_name.len())
         + 1
         + sizeof_len(self.action_digest.get_size())
         + usize::from(self.inline_stdout) * 2
   }

   fn write_message<Backend>(&self, writer: &mut Writer<Backend>) -> Result<()>
   where
      Backend: WriterBackend,
   {
      if !self.instance_name.is_empty() {
         writer.write_with_tag(tag(1, LENGTH), |inner| {
            inner.write_string(&self.instance_name)
         })?;
      }
      writer.write_with_tag(tag(2, LENGTH), |inner| {
         inner.write_message(&self.action_digest)
      })?;
      if self.inline_stdout {
         writer.write_with_tag(tag(3, VARINT), |inner| inner.write_bool(true))?;
      }
      Ok(())
   }
}

#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct UpdateActionResultRequest {
   pub instance_name: String,
   pub action_digest: Digest,
   pub action_result: ActionResult,
}

impl MessageWrite for UpdateActionResultRequest {
   fn get_size(&self) -> usize {
      bytes_size(self.instance_name.len())
         + 1
         + sizeof_len(self.action_digest.get_size())
         + 1
         + sizeof_len(self.action_result.get_size())
   }

   fn write_message<Backend>(&self, writer: &mut Writer<Backend>) -> Result<()>
   where
      Backend: WriterBackend,
   {
      if !self.instance_name.is_empty() {
         writer.write_with_tag(tag(1, LENGTH), |inner| {
            inner.write_string(&self.instance_name)
         })?;
      }
      writer.write_with_tag(tag(2, LENGTH), |inner| {
         inner.write_message(&self.action_digest)
      })?;
      writer.write_with_tag(tag(3, LENGTH), |inner| {
         inner.write_message(&self.action_result)
      })
   }
}

#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct ReadRequest {
   pub resource_name: String,
}

impl MessageWrite for ReadRequest {
   fn get_size(&self) -> usize {
      bytes_size(self.resource_name.len())
   }

   fn write_message<Backend>(&self, writer: &mut Writer<Backend>) -> Result<()>
   where
      Backend: WriterBackend,
   {
      writer.write_with_tag(tag(1, LENGTH), |inner| {
         inner.write_string(&self.resource_name)
      })
   }
}

#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct ReadResponse {
   pub data: Vec<u8>,
}

impl<'bytes> MessageRead<'bytes> for ReadResponse {
   #[expect(
      clippy::renamed_function_params,
      reason = "quick-protobuf names it r, shorter than names here may be"
   )]
   fn from_reader(reader: &mut BytesReader, bytes: &'bytes [u8]) -> Result<Self> {
      let mut message = Self::default();
      while !reader.is_eof() {
         match reader.next_tag(bytes)? {
            82 => reader.read_bytes(bytes)?.clone_into(&mut message.data),
            other => reader.read_unknown(bytes, other)?,
         }
      }
      Ok(message)
   }
}

#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct WriteRequest {
   pub resource_name: String,
   pub write_offset:  i64,
   pub finish_write:  bool,
   pub data:          Vec<u8>,
}

impl MessageWrite for WriteRequest {
   fn get_size(&self) -> usize {
      bytes_size(self.resource_name.len())
         + varint_size(self.write_offset)
         + usize::from(self.finish_write) * 2
         + bytes_size(self.data.len())
   }

   fn write_message<Backend>(&self, writer: &mut Writer<Backend>) -> Result<()>
   where
      Backend: WriterBackend,
   {
      writer.write_with_tag(tag(1, LENGTH), |inner| {
         inner.write_string(&self.resource_name)
      })?;
      if self.write_offset != 0 {
         writer.write_with_tag(tag(2, VARINT), |inner| inner.write_int64(self.write_offset))?;
      }
      if self.finish_write {
         writer.write_with_tag(tag(3, VARINT), |inner| inner.write_bool(true))?;
      }
      if !self.data.is_empty() {
         writer.write_with_tag(tag(10, LENGTH), |inner| inner.write_bytes(&self.data))?;
      }
      Ok(())
   }
}

/// A reply whose fields the remote tier ignores, read only to be skipped.
#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct Ignored;

impl<'bytes> MessageRead<'bytes> for Ignored {
   #[expect(
      clippy::renamed_function_params,
      reason = "quick-protobuf names it r, shorter than names here may be"
   )]
   fn from_reader(reader: &mut BytesReader, bytes: &'bytes [u8]) -> Result<Self> {
      while !reader.is_eof() {
         let next = reader.next_tag(bytes)?;
         reader.read_unknown(bytes, next)?;
      }
      Ok(Self)
   }
}
