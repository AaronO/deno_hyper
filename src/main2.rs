use deno_core::v8;

use std::borrow::Cow;
use std::cell::RefCell;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::rc::Rc;
use std::str::FromStr;

use deno_core::error::bad_resource_id;
use deno_core::error::bail;
use deno_core::error::type_error;
use deno_core::error::AnyError;
use deno_core::futures::Stream;
use deno_core::op_close;
use deno_core::serde_json;
use deno_core::serde_json::json;
use deno_core::serde_json::Value;
use deno_core::AsyncRefCell;
use deno_core::BufVec;
use deno_core::CancelFuture;
use deno_core::CancelHandle;
use deno_core::CancelTryFuture;
use deno_core::JsRuntime;
use deno_core::OpState;
use deno_core::RcRef;
use deno_core::Resource;
use deno_core::ZeroCopyBuf;

use deno_core::futures::StreamExt;
use hyper::body::HttpBody;
use hyper::service::make_service_fn;
use hyper::service::service_fn;
use hyper::Body;
use hyper::Request;
use hyper::Response;
use hyper::Server;
use serde::Deserialize;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_util::io::StreamReader;

const HTTP_ADDR: &str = "127.0.0.1:4000";

pub type RequestAndResponse = (Request<Body>, oneshot::Sender<Response<Body>>);

#[tokio::main]
async fn main2() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let addr: SocketAddr = ([127, 0, 0, 1], 8080).into();

    // needs to be a resource
    let mut tcp_listener = TcpListener::bind(addr).await?;

    loop {
        let (tcp_stream, _) = tcp_listener.accept().await?;
        tokio::task::spawn(async move {
            if let Err(http_err) = Http::new()
                    .http1_only(true)
                    .http1_keep_alive(true)
                    .serve_connection(tcp_stream, service_fn(hello))
                    .await {
                eprintln!("Error while serving HTTP connection: {}", http_err);
            }
        });
    }
}

struct ResponseFuture {
  response: Option<Response>
}

impl Future for ResponseFuture {
  fn poll() {
    if self.response.is_none() {
      Poll::Pending
    } else {
      Poll::Ready(self.response)
    }
  }
}

struct DenoServiceResponse {

}

struct DenoServiceInner {
  request: Option<Request>,
  response_future: Option<ResponseFuture>,
}

struct DenoService {
  inner: Arc<Mutex<DenoServiceInner>,
}

impl DenoService {
  fn take_request() -> Option<Request> {
    todo!()
  }
}

impl Service for DenoService {
  fn poll_ready() {
    todo!()
  }

  fn call(&self, req: Request<>) {
    let inner = self.inner.lock().unwrap();
    inner.request = Some(req);
    let response_future = ResponseFuture {
      response: None
    }.shared();
    inner.response_future = response_future.clone();
    response_future
  }
}

fn accept_op() {
  // TODO: get tcp listener from resource table
  let (tcp_stream, _) = tcp_listener.accept().await?;

  let deno_service = DenoService::default();
  // this is a future that will be polled from JS
  let hyper_conn = Http::new()
    .http1_only(true)
    .http1_keep_alive(true)
    .serve_connection(tcp_stream, deno_service.clone());
  // put hyper_conn in resource table
  let resource = {
    hyper_conn,
    deno_service,
  }
  let rid = 1;
  Ok(rid)
}

fn op_next_request() {
  let resource = resource_table.get(rid);
  poll_fn(|cx| {
    resource.hyper_conn.poll();
    if let Some(request) = resource.deno_service.take_request() {
      return Poll::Ready(request)
    }
    Poll::Pending
  })
}

fn op_respond(buf: &[]) {
  let resource = resource_table.get(rid);

  if let Some(response_future) = resource.deno_service.response_future {
    let mut response = Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(buf);
    response_future.response = Some(response);
  } else {
    panic!();
  }
}

async fn hello(_req: Request<Body>) -> Result<Response<Body>, Infallible> {
   Ok(Response::new(Body::from("Hello World!")))
}
