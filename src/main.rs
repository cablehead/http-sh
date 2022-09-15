use std::convert::Infallible;
use std::net::SocketAddr;

use tokio::signal::unix::{signal, SignalKind};

use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, StatusCode};

async fn handler(req: Request<Body>) -> Result<Response<Body>, hyper::http::Error> {
    match req.method() {
        &hyper::Method::GET => {
            let mut args = vec!["./stream", "cat", "--follow", "--sse"];
            if let Some(last_id) = req.headers().get("Last-Event-ID") {
                args.push("--last-id");
                args.push(last_id.to_str().unwrap());
            }

            println!("{:?}", args);

            let p = tokio::process::Command::new("xs-2")
                .args(args)
                .stdin(std::process::Stdio::null())
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
            let (parts, body) = req.into_parts();

            let re = regex::Regex::new(r"^/pipe/(?P<id>\d+)$").unwrap();
            let id = re
                .captures(parts.uri.path())
                .unwrap()
                .name("id")
                .unwrap()
                .as_str();

            let body = hyper::body::to_bytes(body).await.unwrap().to_vec();
            let body = std::str::from_utf8(&body).unwrap();

            let p = tokio::process::Command::new("xs-2")
                .args(vec!["./stream", "pipe", id, "--", "bash", "-c", body])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("failed to spawn");

            let body = Body::wrap_stream(tokio_util::io::ReaderStream::new(p.stdout.unwrap()));
            let resp = Response::builder()
                .header("Content-Type", "text/plain")
                .header("Access-Control-Allow-Origin", "http://localhost:8000")
                .body(body);
            return resp;
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
    let addr = SocketAddr::from(([127, 0, 0, 1], 8002));
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
        .header("Last-Event-ID", 5)
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
    let req = Request::post("https://www.rust-lang.org/pipe/2")
        .body("cat".into())
        .unwrap();
    let resp = handler(req).await.unwrap();
    println!("{:?}", resp);
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
