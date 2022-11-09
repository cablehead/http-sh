use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;

use futures::TryStreamExt as _;

use tokio::signal::unix::{signal, SignalKind};

use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, StatusCode};

use clap::{AppSettings, Parser};

/*
 * todo:
let params: HashMap<String, String> = req
    .uri()
    .query()
    .map(|v| {
        url::form_urlencoded::parse(v.as_bytes())
            .into_owned()
            .collect()
    })
    .unwrap_or_else(HashMap::new);
*/

/*
if let Some(last_id) = req.headers().get("Last-Event-ID") {
    args.push("--last-id");
    args.push(last_id.to_str().unwrap());
}
*/

/*
let (parts, body) = req.into_parts();
let re = regex::Regex::new(r"^/pipe/(?P<id>\d+)$").unwrap();
let id = re
    .captures(parts.uri.path())
    .unwrap()
    .name("id")
    .unwrap()
    .as_str();
*/

#[derive(Parser, Debug, Clone)]
#[clap(author, version, about, long_about = None)]
#[clap(global_setting(AppSettings::DisableHelpSubcommand))]
struct Args {
    #[clap(short, long, value_parser)]
    port: u16,
    #[clap(value_parser)]
    command: String,
    #[clap(value_parser)]
    args: Vec<String>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let addr = SocketAddr::from(([127, 0, 0, 1], args.port));

    let make_svc = make_service_fn(|_conn| {
        let args = args.clone();
        let svc_fn = service_fn(move |req| {
            let args = args.clone();
            async move {
                Ok::<Response<Body>, Infallible>(handler(req, &args.command, &args.args).await)
            }
        });
        async move { Ok::<_, Infallible>(svc_fn) }
    });

    let server = hyper::Server::bind(&addr).serve(make_svc);

    let graceful = server.with_graceful_shutdown(shutdown_signal());
    if let Err(e) = graceful.await {
        eprintln!("server error: {}", e);
    }
}

async fn handler(req: Request<Body>, command: &String, args: &Vec<String>) -> Response<Body> {
    println!("{:?}", req);

    fn headers_to_hashmap(headers: &hyper::header::HeaderMap) -> HashMap<String, Vec<String>> {
        let mut ret = std::collections::HashMap::new();
        for (k, v) in headers {
            let k = k.as_str().to_owned();
            let v = String::from_utf8_lossy(v.as_bytes()).into_owned();
            ret.entry(k).or_insert_with(Vec::new).push(v)
        }
        ret
    }

    let mut p = tokio::process::Command::new(command)
        .args(args)
        .env("HTTP_SH_METHOD", req.method().as_str().to_ascii_uppercase())
        .env("HTTP_SH_URI", req.uri().to_string())
        .env(
            "HTTP_SH_HEADERS",
            serde_json::to_string(&headers_to_hashmap(&req.headers())).unwrap(),
        )
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn");

    let req_body = req
        .into_body()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e));
    let mut req_body = tokio_util::io::StreamReader::new(req_body);
    let mut stdin = p.stdin.take().expect("failed to take stdin");
    let write_stdin = async {
        tokio::io::copy(&mut req_body, &mut stdin)
            .await
            .expect("streaming request body");
    };

    let mut stdout = p.stdout.take().expect("failed to take stdout");
    let res_body = Body::wrap_stream(tokio_util::io::ReaderStream::new(stdout));
    let read_stdout = async {
        // todo: this should not return until the body stream has ended
        Response::builder()
            .header("Content-Type", "text/plain")
            .body(res_body)
            .expect("streaming response body")
    };

    let (_, response) = tokio::join!(write_stdin, read_stdout);

    tokio::spawn(async move {
        p.wait().await.expect("process exited with an error");
    });

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

#[tokio::test]
async fn test_handler() {
    let req = Request::post("https://www.rust-lang.org/")
        .header("Last-Event-ID", 5)
        .body("zebody".into())
        .unwrap();
    let resp = handler(req, &"bash".into(), &vec!["-c".into(), "cat".into()]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get("content-type").unwrap(), "text/plain");
    let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
    let body = std::str::from_utf8(&body).unwrap();
    assert_eq!(body, "zebody");
}
