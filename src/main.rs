use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;

use futures::TryStreamExt as _;

use tokio::signal::unix::{signal, SignalKind};

use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, StatusCode};

use clap::{AppSettings, Parser};

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
#[clap(global_setting(AppSettings::DisableHelpSubcommand))]
struct Args {
    #[clap(short, long, value_parser)]
    port: u16,
}

async fn handler(req: Request<Body>) -> Result<Response<Body>, hyper::http::Error> {
    println!("{:?}", req);
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

        &hyper::Method::OPTIONS => {
            return Response::builder()
                .header("Access-Control-Allow-Origin", "http://localhost:8000")
                .header("Access-Control-Allow-Methods", "*")
                .status(StatusCode::NO_CONTENT)
                .body(Body::empty());
        }

        &hyper::Method::PUT => {
            let params: HashMap<String, String> = req
                .uri()
                .query()
                .map(|v| {
                    url::form_urlencoded::parse(v.as_bytes())
                        .into_owned()
                        .collect()
                })
                .unwrap_or_else(HashMap::new);

            let mut cmd = tokio::process::Command::new("xs-2");
            cmd.args(vec!["./stream", "put"]);
            params.get("source_id").map(|x| cmd.args(vec!["--source-id", x]));
            params.get("topic").map(|x| cmd.args(vec!["--topic", x]));
            params.get("attribute").map(|x| cmd.args(vec!["--attribute", x]));
            let mut p = cmd
                .stdin(std::process::Stdio::piped())
                .spawn()
                .expect("failed to spawn");

            let body = req
                .into_body()
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e));
            let mut body = tokio_util::io::StreamReader::new(body);
            let mut stdin = p.stdin.take().expect("failed to open stdin");
            tokio::io::copy(&mut body, &mut stdin)
                .await
                .expect("pipe failed");
            return Response::builder()
                .header("Content-Type", "text/plain")
                .header("Access-Control-Allow-Origin", "http://localhost:8000")
                .body("ok".into());
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
    let args = Args::parse();
    let addr = SocketAddr::from(([127, 0, 0, 1], args.port));
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
