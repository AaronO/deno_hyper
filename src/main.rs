use deno_core::v8;

use std::borrow::Cow;
use std::cell::RefCell;
// use std::convert::Infallible;
// use std::net::SocketAddr;
use std::pin::Pin;
use std::rc::Rc;
// use std::str::FromStr;
use std::future::Future;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;

use deno_core::error::bad_resource_id;
use deno_core::error::AnyError;
use deno_core::futures::future::poll_fn;
use deno_core::futures::FutureExt;
use deno_core::futures::Stream;
use deno_core::op_close;
use deno_core::AsyncRefCell;
use deno_core::CancelFuture;
use deno_core::CancelHandle;
use deno_core::CancelTryFuture;
use deno_core::JsRuntime;
use deno_core::OpState;
use deno_core::OpBuf;
use deno_core::RcRef;
use deno_core::Resource;
use deno_core::ResourceId;

use hyper::http;
use hyper::server::conn::Connection;
use hyper::server::conn::Http;
use hyper::service::Service;
use hyper::Body;
use hyper::Request;
use hyper::Response;
use serde::Deserialize;
use serde::Serialize;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio_util::io::StreamReader;

const HTTP_ADDR: &str = "127.0.0.1:4000";

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), AnyError> {
  for arg in std::env::args() {
    v8::V8::set_flags_from_string(&arg);
  }

  let mut js_runtime = create_js_runtime();

  js_runtime
    .execute("bootstrap.js", include_str!("bootstrap.js"))
    .unwrap();

  let script = tokio::fs::read_to_string("mod.js").await.unwrap();

  js_runtime.execute("mod.js", &script).unwrap();
  js_runtime.run_event_loop().await.unwrap();

  Ok(())
}

fn create_js_runtime() -> JsRuntime {
  let mut js_runtime = JsRuntime::new(Default::default());
  js_runtime.register_op(
    "op_create_server",
    deno_core::json_op_async(op_create_server),
  );
  js_runtime.register_op("op_accept", deno_core::json_op_async(op_accept));
  js_runtime
    .register_op("op_next_request", deno_core::json_op_async(op_next_request));
  js_runtime.register_op("op_respond", deno_core::json_op_sync(op_respond));
  // js_runtime
  //   .register_op("op_request_read", deno_core::json_op_async(op_request_read));
  // js_runtime.register_op(
  //   "op_response_write",
  //   deno_core::json_op_async(op_response_write),
  // );
  js_runtime.register_op("op_close", deno_core::json_op_sync(op_close));
  js_runtime
}

#[derive(Serialize)]
pub struct NextRequest {
  pub method: String,
  pub headers: Vec<(String, String)>,
  pub url: String,
}

pub async fn op_next_request(
  state: Rc<RefCell<OpState>>,
  rid: ResourceId,
  _data: OpBuf,
) -> Result<NextRequest, AnyError> {
  let conn_resource = state
    .borrow()
    .resource_table
    .get::<ConnResource>(rid)
    .ok_or_else(bad_resource_id)?;

  poll_fn(|cx| {
    // TODO: error is swallowed and when connection shutdowns we continue going
    let _ = conn_resource.hyper_connection.borrow_mut().poll_unpin(cx);

    if let Some(req) = conn_resource
      .deno_service
      .inner
      .lock()
      .unwrap()
      .request
      .take()
    {
      let method = req.method().to_string();

      let mut headers = Vec::with_capacity(req.headers().len());

      for (name, value) in req.headers().iter() {
        let name = name.to_string();
        let value = value.to_str().unwrap_or("").to_string();
        headers.push((name, value));
      }

      let host = extract_host(&req).expect("HTTP request without Host header");
      let path = req.uri().path_and_query().unwrap();
      let url = format!("https://{}{}", host, path);

      let req_json = NextRequest{
        method,
        headers,
        url,
      };

      return Poll::Ready(Ok(req_json));
    }

    Poll::Pending
  })
  .await
}

#[derive(Default)]
struct DenoServiceInner {
  request: Option<Request<Body>>,
  response: Option<Response<Body>>,
}

#[derive(Clone, Default)]
struct DenoService {
  inner: Arc<Mutex<DenoServiceInner>>,
  waker: Option<Waker>,
}

impl Service<Request<Body>> for DenoService {
  type Response = Response<Body>;
  type Error = http::Error;
  type Future =
    Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

  fn poll_ready(
    &mut self,
    _cx: &mut Context<'_>,
  ) -> Poll<Result<(), Self::Error>> {
    // TODO:
    Poll::Ready(Ok(()))
  }

  fn call(&mut self, req: Request<Body>) -> Self::Future {
    {
      let mut inner = self.inner.lock().unwrap();
      inner.request = Some(req);
    }

    let inner = self.inner.clone();

    let mut self_ = self.clone();
    poll_fn(move |cx| {
      // eprintln!("attach waker");
      self_.waker = Some(cx.waker().clone());
      let inner = inner.clone();
      let mut guard = inner.lock().unwrap();
      if let Some(response) = guard.response.take() {
        return Poll::Ready(Ok(response));
      }
      Poll::Pending
    })
    .boxed()
  }
}

struct HttpServer {
  pub listener: TcpListener,
}

impl Resource for HttpServer {
  fn name(&self) -> Cow<str> {
    "httpServer".into()
  }
}

struct ConnResource {
  pub hyper_connection:
    Rc<RefCell<Connection<tokio::net::TcpStream, DenoService>>>,
  pub deno_service: DenoService,
}

impl Resource for ConnResource {
  fn name(&self) -> Cow<str> {
    "httpConnection".into()
  }
}

pub async fn op_create_server(
  state: Rc<RefCell<OpState>>,
  _value: (),
  _data: OpBuf,
) -> Result<ResourceId, AnyError> {
  // TODO: handle address
  let tcp_listener = TcpListener::bind(HTTP_ADDR).await.unwrap();
  let http_server = HttpServer {
    listener: tcp_listener,
  };

  let rid = state.borrow_mut().resource_table.add(http_server);

  Ok(rid)
}

pub async fn op_accept(
  state: Rc<RefCell<OpState>>,
  rid: ResourceId,
  _data: OpBuf,
) -> Result<ResourceId, AnyError> {

  let http_server = state
    .borrow()
    .resource_table
    .get::<HttpServer>(rid)
    .ok_or_else(bad_resource_id)?;

  let (tcp_stream, _) = http_server.listener.accept().await?;
  let deno_service = DenoService::default();

  let hyper_connection =
    Http::new().serve_connection(tcp_stream, deno_service.clone());

  let conn_resource = ConnResource {
    hyper_connection: Rc::new(RefCell::new(hyper_connection)),
    deno_service,
  };
  let rid = state.borrow_mut().resource_table.add(conn_resource);

  Ok(rid)
}


#[derive(Deserialize)]
pub struct RespondArgs {
  pub rid: u32,
  pub status: u16,
  pub headers: Vec<(String, String)>,
}

pub fn op_respond(
  state: &mut OpState,
  args: RespondArgs,
  _data: OpBuf,
) -> Result<(), AnyError> {

  let RespondArgs {
    rid,
    status,
    headers,
  } = args;

  let conn_resource = state
    .resource_table
    .get::<ConnResource>(rid)
    .ok_or_else(bad_resource_id)?;

  {
    let mut deno_service = conn_resource.deno_service.inner.lock().unwrap();
    let mut builder = Response::builder().status(status);

    for (name, value) in headers {
      builder = builder.header(&name, &value);
    }
    let response = builder.body(Body::from("hello world"))?;

    deno_service.response = Some(response);
  }
  if let Some(waker) = conn_resource.deno_service.waker.as_ref() {
    waker.wake_by_ref();
  }

  Ok(())
}

pub async fn op_request_read(
  state: Rc<RefCell<OpState>>,
  rid: ResourceId,
  data: OpBuf,
) -> Result<usize, AnyError> {
  if data.is_none() {
    panic!("Invalid number of arguments");
  }
  let data = data.unwrap();

  let resource = state
    .borrow()
    .resource_table
    .get::<RequestBodyResource>(rid as u32)
    .ok_or_else(bad_resource_id)?;
  let mut reader = RcRef::map(&resource, |r| &r.reader).borrow_mut().await;
  let cancel = RcRef::map(resource, |r| &r.cancel);
  let mut buf = data.clone();
  let read = reader.read(&mut buf).try_or_cancel(cancel).await?;
  Ok(read)
}

pub async fn op_response_write(
  state: Rc<RefCell<OpState>>,
  rid: ResourceId,
  data: OpBuf,
) -> Result<(), AnyError> {
  let buf = match data {
    Some(buf) => Vec::from(&*buf),
    None => panic!("Invalid number of arguments"),
  };

  let resource = state
    .borrow()
    .resource_table
    .get::<DyperResponseBodyResource>(rid as u32)
    .ok_or_else(bad_resource_id)?;
  let mut body = RcRef::map(&resource, |r| &r.body).borrow_mut().await;
  let cancel = RcRef::map(resource, |r| &r.cancel);
  body.send_data(buf.into()).or_cancel(cancel).await??;

  Ok(())
}

type BytesStream =
  Pin<Box<dyn Stream<Item = Result<bytes::Bytes, std::io::Error>> + Unpin>>;

struct RequestBodyResource {
  reader: AsyncRefCell<StreamReader<BytesStream, bytes::Bytes>>,
  cancel: CancelHandle,
}

impl Resource for RequestBodyResource {
  fn name(&self) -> Cow<str> {
    "requestBody".into()
  }
}

struct DyperResponseBodyResource {
  body: AsyncRefCell<hyper::body::Sender>,
  cancel: CancelHandle,
}

impl Resource for DyperResponseBodyResource {
  fn name(&self) -> Cow<str> {
    "requestBody".into()
  }
}

fn extract_host(req: &Request<Body>) -> Option<String> {
  if req.version() == hyper::Version::HTTP_2 {
    req.uri().host().map(|s| {
      format!(
        "{}{}",
        s,
        if let Some(port) = req.uri().port_u16() {
          format!(":{}", port)
        } else {
          "".to_string()
        }
      )
    })
  } else {
    let host_header = req.headers().get(hyper::header::HOST)?;
    Some(host_header.to_str().ok()?.to_string())
  }
}
