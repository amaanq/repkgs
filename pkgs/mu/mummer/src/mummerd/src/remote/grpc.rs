//! A gRPC channel over one HTTP/2 connection, plain or TLS, reopened after
//! a call fails.

use std::{
   future::poll_fn,
   io,
   path::Path,
   sync::{
      Arc,
      Mutex,
   },
   time::Duration,
};

use bytes::{
   BufMut as _,
   Bytes,
   BytesMut,
};
use h2::{
   SendStream,
   client::{
      SendRequest,
      handshake,
   },
};
use http::Request;
use quick_protobuf::{
   BytesReader,
   MessageRead,
   MessageWrite,
   Writer,
};
use rustls::{
   ClientConfig,
   RootCertStore,
   crypto::ring::default_provider,
   pki_types::{
      CertificateDer,
      PrivateKeyDer,
      ServerName,
      pem::PemObject as _,
   },
};
use tokio::{
   io::{
      AsyncRead,
      AsyncWrite,
   },
   net::TcpStream,
   time::timeout,
};
use tokio_rustls::TlsConnector;

const CONNECT: Duration = Duration::from_secs(10);
const CALL: Duration = Duration::from_secs(60);

/// The gRPC status codes the remote tier tells apart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Code {
   Ok,
   NotFound,
   Other(u32),
}

impl From<u32> for Code {
   fn from(code: u32) -> Self {
      match code {
         0 => Self::Ok,
         5 => Self::NotFound,
         other => Self::Other(other),
      }
   }
}

pub struct Channel {
   authority: String,
   host:      String,
   tls:       Option<TlsConnector>,
   sender:    Mutex<Option<SendRequest<Bytes>>>,
}

impl Channel {
   pub fn new(authority: &str, tls: Option<TlsConnector>) -> Self {
      let host = authority
         .rsplit_once(':')
         .map_or(authority, |(host, _)| host)
         .trim_start_matches('[')
         .trim_end_matches(']');
      Self {
         authority: authority.to_owned(),
         host: host.to_owned(),
         tls,
         sender: Mutex::new(None),
      }
   }

   /// One call, its requests sent as a client stream and every reply read,
   /// with the status from the trailers.
   pub async fn call<Sent, Received>(
      &self,
      method: &str,
      requests: &[Sent],
   ) -> io::Result<(Code, Vec<Received>)>
   where
      Sent: MessageWrite + Sync,
      Received: for<'bytes> MessageRead<'bytes> + Send,
   {
      let result = timeout(CALL, self.exchange(method, requests))
         .await
         .map_err(|_elapsed| io::Error::new(io::ErrorKind::TimedOut, "remote call timed out"))
         .flatten();
      if result.is_err()
         && let Ok(mut sender) = self.sender.lock()
      {
         *sender = None;
      }
      result
   }

   async fn exchange<Sent, Received>(
      &self,
      method: &str,
      requests: &[Sent],
   ) -> io::Result<(Code, Vec<Received>)>
   where
      Sent: MessageWrite + Sync,
      Received: for<'bytes> MessageRead<'bytes> + Send,
   {
      let mut sender = self
         .sender()
         .await?
         .ready()
         .await
         .map_err(io::Error::other)?;
      let scheme = if self.tls.is_some() { "https" } else { "http" };
      let request = Request::post(format!("{scheme}://{}/{method}", self.authority))
         .header("content-type", "application/grpc")
         .header("te", "trailers")
         .body(())
         .map_err(io::Error::other)?;
      let (response, mut stream) = sender
         .send_request(request, false)
         .map_err(io::Error::other)?;
      let mut body = BytesMut::new();
      for message in requests {
         let mut encoded = Vec::with_capacity(message.get_size());
         message
            .write_message(&mut Writer::new(&mut encoded))
            .map_err(io::Error::other)?;
         body.put_u8(0);
         body.put_u32(u32::try_from(encoded.len()).map_err(io::Error::other)?);
         body.put_slice(&encoded);
      }
      send_all(&mut stream, body.freeze()).await?;
      let (parts, mut incoming) = response.await.map_err(io::Error::other)?.into_parts();
      let mut data = BytesMut::new();
      while let Some(received) = incoming.data().await {
         let chunk = received.map_err(io::Error::other)?;
         incoming
            .flow_control()
            .release_capacity(chunk.len())
            .map_err(io::Error::other)?;
         data.put_slice(&chunk);
      }
      let ending = incoming.trailers().await.map_err(io::Error::other)?;
      let code = ending
         .as_ref()
         .and_then(|trailers| trailers.get("grpc-status"))
         .or_else(|| parts.headers.get("grpc-status"))
         .and_then(|value| value.to_str().ok()?.parse::<u32>().ok())
         .ok_or_else(|| io::Error::other(format!("{method} answered without a grpc-status")))?;
      let replies = frames(&data)?
         .into_iter()
         .map(|frame| {
            Received::from_reader(&mut BytesReader::from_bytes(frame), frame)
               .map_err(io::Error::other)
         })
         .collect::<io::Result<Vec<_>>>()?;
      Ok((Code::from(code), replies))
   }

   async fn sender(&self) -> io::Result<SendRequest<Bytes>> {
      if let Some(ref sender) = *self.sender.lock().map_err(|_poisoned| poisoned())? {
         return Ok(sender.clone());
      }
      let tcp = timeout(CONNECT, TcpStream::connect(&self.authority))
         .await
         .map_err(|_elapsed| {
            io::Error::new(io::ErrorKind::TimedOut, "remote connect timed out")
         })??;
      tcp.set_nodelay(true)?;
      let sender = if let Some(ref tls) = self.tls {
         let name = ServerName::try_from(self.host.clone()).map_err(io::Error::other)?;
         open(tls.connect(name, tcp).await?).await?
      } else {
         open(tcp).await?
      };
      *self.sender.lock().map_err(|_poisoned| poisoned())? = Some(sender.clone());
      Ok(sender)
   }
}

/// TLS checked against `ca`, else the web's roots, presenting `identity`, a
/// PEM file with a certificate chain and its key, when given.
pub fn tls(ca: Option<&Path>, identity: Option<&Path>) -> io::Result<TlsConnector> {
   let mut roots = RootCertStore::empty();
   if let Some(path) = ca {
      for certificate in CertificateDer::pem_file_iter(path).map_err(io::Error::other)? {
         roots
            .add(certificate.map_err(io::Error::other)?)
            .map_err(io::Error::other)?;
      }
   } else {
      roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
   }
   let builder = ClientConfig::builder_with_provider(Arc::new(default_provider()))
      .with_safe_default_protocol_versions()
      .map_err(io::Error::other)?
      .with_root_certificates(roots);
   let mut config = if let Some(path) = identity {
      let chain = CertificateDer::pem_file_iter(path)
         .map_err(io::Error::other)?
         .collect::<Result<Vec<_>, _>>()
         .map_err(io::Error::other)?;
      let key = PrivateKeyDer::from_pem_file(path).map_err(io::Error::other)?;
      builder
         .with_client_auth_cert(chain, key)
         .map_err(io::Error::other)?
   } else {
      builder.with_no_client_auth()
   };
   config.alpn_protocols = vec![b"h2".to_vec()];
   Ok(TlsConnector::from(Arc::new(config)))
}

async fn open<Stream>(stream: Stream) -> io::Result<SendRequest<Bytes>>
where
   Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
   let (sender, connection) = handshake(stream).await.map_err(io::Error::other)?;
   tokio::spawn(async move {
      drop(connection.await);
   });
   Ok(sender)
}

/// Sends `data` as the whole request body within HTTP/2 flow control.
async fn send_all(stream: &mut SendStream<Bytes>, mut data: Bytes) -> io::Result<()> {
   if data.is_empty() {
      return stream
         .send_data(Bytes::new(), true)
         .map_err(io::Error::other);
   }
   while !data.is_empty() {
      stream.reserve_capacity(data.len());
      let granted = poll_fn(|context| stream.poll_capacity(context))
         .await
         .ok_or_else(|| io::Error::other("remote closed the request stream"))?
         .map_err(io::Error::other)?;
      let chunk = data.split_to(granted.min(data.len()));
      stream
         .send_data(chunk, data.is_empty())
         .map_err(io::Error::other)?;
   }
   Ok(())
}

/// The length-prefixed messages of a gRPC body.
fn frames(mut data: &[u8]) -> io::Result<Vec<&[u8]>> {
   let mut found = Vec::new();
   while !data.is_empty() {
      let (header, rest) = data
         .split_at_checked(5)
         .ok_or_else(|| io::Error::other("truncated gRPC message header"))?;
      let &[compressed, first, second, third, fourth] = header else {
         return Err(io::Error::other("truncated gRPC message header"));
      };
      if compressed != 0 {
         return Err(io::Error::other("compressed gRPC message"));
      }
      let length = usize::try_from(u32::from_be_bytes([first, second, third, fourth]))
         .map_err(io::Error::other)?;
      let (message, remaining) = rest
         .split_at_checked(length)
         .ok_or_else(|| io::Error::other("truncated gRPC message"))?;
      found.push(message);
      data = remaining;
   }
   Ok(found)
}

fn poisoned() -> io::Error {
   io::Error::other("remote connection lock poisoned")
}
