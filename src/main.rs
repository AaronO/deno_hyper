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

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), AnyError> {
  let mut js_runtime = create_js_runtime();
  let (tx, rx) = mpsc::unbounded_channel::<RequestAndResponse>();
  let op_state = js_runtime.op_state();
  let rx = Rc::new(RefCell::new(rx));
  op_state.borrow_mut().put(rx);

  js_runtime
    .execute("bootstrap.js", include_str!("bootstrap.js"))
    .unwrap();

  let script = tokio::fs::read_to_string("mod.js").await.unwrap();
  js_runtime.execute("mod.js", &script).unwrap();

  tokio::spawn(async move {
    let make_svc = make_service_fn(|_| {
      let tx = tx.clone();
      async move {
        Ok::<_, Infallible>(service_fn(move |req: Request<Body>| {
          let tx = tx.clone();
          async move {
            let (resp_tx, resp_rx) = oneshot::channel();
            tx.send((req, resp_tx)).unwrap();
            let resp = resp_rx.await.unwrap();
            Ok::<Response<Body>, Infallible>(resp)
          }
        }))
      }
    });

    let addr = SocketAddr::from_str(HTTP_ADDR).unwrap();
    let builder = Server::bind(&addr);
    let server = builder.serve(make_svc);
    println!("HTTP address: http://{}", server.local_addr());

    server.await.unwrap();
  });

  js_runtime.run_event_loop().await.unwrap();

  Ok(())
}

fn create_js_runtime() -> JsRuntime {
  let mut js_runtime = JsRuntime::new(Default::default());
  js_runtime
    .register_op("op_next_request", deno_core::json_op_async(op_next_request));
  js_runtime.register_op("op_respond", deno_core::json_op_sync(op_respond));
  js_runtime
    .register_op("op_request_read", deno_core::json_op_async(op_request_read));
  js_runtime.register_op(
    "op_response_write",
    deno_core::json_op_async(op_response_write),
  );
  js_runtime.register_op("op_close", deno_core::json_op_sync(op_close));
  js_runtime
}

#[allow(clippy::await_holding_refcell_ref)]
pub async fn op_next_request(
  state: Rc<RefCell<OpState>>,
  _args: Value,
  _data: BufVec,
) -> Result<Value, AnyError> {
  let rx = state
    .borrow()
    .borrow::<Rc<RefCell<mpsc::UnboundedReceiver<RequestAndResponse>>>>()
    .clone();
  let (req, tx) = match rx.borrow_mut().recv().await {
    None => bail!("failed to recieve"),
    Some(v) => v,
  };

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

  let has_body = if let Some(exact_size) = req.size_hint().exact() {
    exact_size > 0
  } else {
    true
  };

  let maybe_request_body_rid = if has_body {
    let stream: BytesStream = Box::pin(req.into_body().map(|r| {
      r.map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))
    }));
    let stream_reader = StreamReader::new(stream);
    let mut state = state.borrow_mut();
    let request_body_rid = state.resource_table.add(RequestBodyResource {
      reader: AsyncRefCell::new(stream_reader),
      cancel: CancelHandle::default(),
    });
    Some(request_body_rid)
  } else {
    None
  };

  let mut state = state.borrow_mut();
  let response_sender_rid =
    state.resource_table.add(ResponseSenderResource(tx));

  let req_json = json!({
    "requestBodyRid": maybe_request_body_rid,
    "responseSenderRid": response_sender_rid,
    "method": method,
    "headers": headers,
    "url": url,
  });

  Ok(req_json)
}

pub fn op_respond(
  state: &mut OpState,
  value: Value,
  data: &mut [ZeroCopyBuf],
) -> Result<Value, AnyError> {
  #[derive(Deserialize)]
  struct Args {
    rid: u32,
    status: u16,
    headers: Vec<(String, String)>,
  }

  let Args {
    rid,
    status,
    headers,
  } = serde_json::from_value(value)?;

  let response_sender = state
    .resource_table
    .take::<ResponseSenderResource>(rid)
    .ok_or_else(bad_resource_id)?;
  let response_sender = Rc::try_unwrap(response_sender)
    .ok()
    .expect("multiple op_respond ongoing");

  let mut builder = Response::builder().status(status);

  for (name, value) in headers {
    builder = builder.header(&name, &value);
  }

  let res;
  let maybe_response_body_rid = match data.len() {
    0 => {
      // If no body is passed, we return a writer for streaming the body.
      let (sender, body) = Body::channel();
      res = builder.body(body)?;

      let response_body_rid =
        state.resource_table.add(DyperResponseBodyResource {
          body: AsyncRefCell::new(sender),
          cancel: CancelHandle::default(),
        });

      Some(response_body_rid)
    }
    1 => {
      // If a body is passed, we use it, and don't return a body for streaming.
      res = builder.body(Vec::from(&*data[0]).into())?;
      None
    }
    _ => panic!("Invalid number of arguments"),
  };

  // oneshot::Sender::send(v) returns |v| on error, not an error object.
  // The only failure mode is the receiver already having dropped its end
  // of the channel.
  if response_sender.0.send(res).is_err() {
    eprintln!("op_respond: receiver dropped");
    return Err(type_error("internal communication error"));
  }

  Ok(serde_json::json!({
    "responseBodyRid": maybe_response_body_rid
  }))
}

pub async fn op_request_read(
  state: Rc<RefCell<OpState>>,
  args: Value,
  data: BufVec,
) -> Result<Value, AnyError> {
  #[derive(Deserialize)]
  #[serde(rename_all = "camelCase")]
  struct Args {
    rid: u32,
  }

  let args: Args = serde_json::from_value(args)?;
  let rid = args.rid;

  if data.len() != 1 {
    panic!("Invalid number of arguments");
  }

  let resource = state
    .borrow()
    .resource_table
    .get::<RequestBodyResource>(rid as u32)
    .ok_or_else(bad_resource_id)?;
  let mut reader = RcRef::map(&resource, |r| &r.reader).borrow_mut().await;
  let cancel = RcRef::map(resource, |r| &r.cancel);
  let mut buf = data[0].clone();
  let read = reader.read(&mut buf).try_or_cancel(cancel).await?;
  Ok(json!({ "read": read }))
}

pub async fn op_response_write(
  state: Rc<RefCell<OpState>>,
  args: Value,
  data: BufVec,
) -> Result<Value, AnyError> {
  #[derive(Deserialize)]
  #[serde(rename_all = "camelCase")]
  struct Args {
    rid: u32,
  }

  let args: Args = serde_json::from_value(args)?;
  let rid = args.rid;

  let buf = match data.len() {
    1 => Vec::from(&*data[0]),
    _ => panic!("Invalid number of arguments"),
  };

  let resource = state
    .borrow()
    .resource_table
    .get::<DyperResponseBodyResource>(rid as u32)
    .ok_or_else(bad_resource_id)?;
  let mut body = RcRef::map(&resource, |r| &r.body).borrow_mut().await;
  let cancel = RcRef::map(resource, |r| &r.cancel);
  body.send_data(buf.into()).or_cancel(cancel).await??;

  Ok(json!({}))
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

struct ResponseSenderResource(oneshot::Sender<Response<Body>>);

impl Resource for ResponseSenderResource {
  fn name(&self) -> Cow<str> {
    "responseSender".into()
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
