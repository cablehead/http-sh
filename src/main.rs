use std::convert::Infallible;
use std::net::SocketAddr;

use futures::TryStreamExt as _;

use tokio::signal::unix::{signal, SignalKind};

use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, StatusCode};

async fn handler(req: Request<Body>) -> Result<Response<Body>, hyper::http::Error> {
    match req.method() {
        &hyper::Method::GET => {
            let p = tokio::process::Command::new("./run.sh")
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("failed to spawn");

            let body = Body::wrap_stream(tokio_util::io::ReaderStream::new(p.stdout.unwrap()));
            let resp = Response::builder()
                .header("Content-Type", "text/event-stream")
                .header("Access-Control-Allow-Origin", "http://localhost:8000")
                .body(body);
            return resp;
        }

        &hyper::Method::POST => {
            let mut p = tokio::process::Command::new("./post.sh")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .expect("failed to spawn");

            let body = req.into_body().map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e));
            let mut body = tokio_util::io::StreamReader::new(body);
            let mut stdin = p.stdin.take().expect("failed to open stdin");
            tokio::io::copy(&mut body, &mut stdin).await.expect("pipe failed");
            return Response::builder().body("ok".into());
        }

        _ => {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body("not found".into());
        }
    }
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
    let req = Request::post("https://www.rust-lang.org/")
        .body("payload".into())
        .unwrap();
    let resp = handler(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_404() {
    let req = Request::delete("https://www.rust-lang.org/")
        .body(Body::empty())
        .unwrap();
    let resp = handler(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
