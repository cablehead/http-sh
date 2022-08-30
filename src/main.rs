use std::convert::Infallible;
use std::net::SocketAddr;

use hyper::service::{make_service_fn, service_fn};
use tokio::signal::unix::{signal, SignalKind};

async fn handler(
    req: hyper::Request<hyper::Body>,
) -> Result<hyper::Response<hyper::Body>, hyper::http::Error> {
    let p = tokio::process::Command::new("./run.sh")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn");

    let body = hyper::Body::wrap_stream(tokio_util::io::ReaderStream::new(p.stdout.unwrap()));
    let response = hyper::Response::builder()
        .header("Content-Type", "text/event-stream")
        .header("Access-Control-Allow-Origin", "http://localhost:8000")
        .body(body);
    response
}

async fn shutdown_signal() {
    let mut sigint = signal(SignalKind::interrupt()).unwrap();
    let mut sigterm = signal(SignalKind::terminate()).unwrap();
    tokio::select! {
        _ = sigint.recv() => {},
        _ = sigterm.recv() => {},
    }
}

#[tokio::main]
async fn main() {
    let addr = SocketAddr::from(([127, 0, 0, 1], 8001));
    let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handler)) });
    let server = hyper::Server::bind(&addr).serve(make_svc);
    let graceful = server.with_graceful_shutdown(shutdown_signal());
    if let Err(e) = graceful.await {
        eprintln!("server error: {}", e);
    }
}

#[tokio::test]
async fn test_handler() {
    let req = hyper::Request::get("https://www.rust-lang.org/")
        .body(hyper::Body::empty())
        .unwrap();
    let resp = handler(req).await.unwrap();
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "text/event-stream",
    );
}
