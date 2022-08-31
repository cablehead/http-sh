use std::convert::Infallible;
use std::net::SocketAddr;

use hyper::service::{make_service_fn, service_fn};
use tokio::signal::unix::{signal, SignalKind};
use hyper::{Request, Response, Body, StatusCode};

async fn handler(
    req: Request<Body>,
) -> Result<Response<Body>, hyper::http::Error> {
    match req.method() {
        &hyper::Method::GET => println!("hi: GET"),
        &hyper::Method::POST => println!("hi: POST"),
        _ => {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body("not found".into());
        }
    }

    let p = tokio::process::Command::new("./run.sh")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn");

    let body = Body::wrap_stream(tokio_util::io::ReaderStream::new(p.stdout.unwrap()));
    let response = Response::builder()
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
async fn test_handler_get() {
    let req = Request::get("https://www.rust-lang.org/")
        .body(Body::empty())
        .unwrap();
    let resp = handler(req).await.unwrap();
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "text/event-stream",
    );
}

#[tokio::test]
async fn test_handler_post() {
    let req = Request::get("https://www.rust-lang.org/")
        .body(Body::empty())
        .unwrap();
    let resp = handler(req).await.unwrap();
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "text/event-stream",
    );
}

#[tokio::test]
async fn test_handler_404() {
    let req = Request::delete("https://www.rust-lang.org/")
    .body(Body::empty())
    .unwrap();
    let resp = handler(req).await.unwrap();
    println!("RESP: {:?}", resp);
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
