use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;

use futures::TryStreamExt as _;

use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::signal::unix::{signal, SignalKind};

use hyper::service::{make_service_fn, service_fn};

use serde::{Deserialize, Serialize};

use clap::{AppSettings, Parser};

/*
 * todo:
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
                Ok::<hyper::Response<hyper::Body>, Infallible>(
                    handler(req, &args.command, &args.args).await,
                )
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

async fn handler(
    req: hyper::Request<hyper::Body>,
    command: &String,
    args: &Vec<String>,
) -> hyper::Response<hyper::Body> {
    #[derive(Serialize, Deserialize)]
    struct Request {
        request_id: scru128::Scru128Id,
        #[serde(with = "http_serde::method")]
        method: http::method::Method,
        #[serde(with = "http_serde::header_map")]
        headers: http::header::HeaderMap,
        #[serde(with = "http_serde::uri")]
        uri: http::Uri,
        path: String,
        query: HashMap<String, String>,
    }

    #[derive(Default, Debug, Serialize, Deserialize)]
    struct Response {
        status: Option<u16>,
        headers: Option<std::collections::HashMap<String, String>>,
    }

    let mut p = tokio::process::Command::new(command)
        .args(args)
        /*
         * todo: as a cli option?
        .env("HTTP_SH_METHOD", req.method().as_str().to_ascii_uppercase())
        .env("HTTP_SH_URI", req.uri().to_string())
        .env(
            "HTTP_SH_HEADERS",
            serde_json::to_string(&headers_to_hashmap(&req.headers())).unwrap(),
        )
        */
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn");

    let (req_parts, req_body) = req.into_parts();

    let path = req_parts.uri.path().to_string();

    let query: HashMap<String, String> = req_parts
        .uri
        .query()
        .map(|v| {
            url::form_urlencoded::parse(v.as_bytes())
                .into_owned()
                .collect()
        })
        .unwrap_or_else(HashMap::new);

    let req_meta = serde_json::json!(Request {
        request_id: scru128::new(),
        method: req_parts.method,
        headers: req_parts.headers,
        uri: req_parts.uri,
        path: path,
        query: query,
    });

    println!("{}", req_meta);

    let write_stdin = async {
        let req_body = req_body.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e));
        let mut req_body = tokio_util::io::StreamReader::new(req_body);

        let mut stdin = p.stdin.take().expect("failed to take stdin");
        stdin
            .write_all(format!("{}\n", &req_meta.to_string()).as_bytes())
            .await
            .unwrap();
        tokio::io::copy(&mut req_body, &mut stdin)
            .await
            .expect("streaming request body");
    };

    let read_stdout = async {
        let stdout = p.stdout.take().expect("failed to take stdout");
        let mut stdout = tokio::io::BufReader::new(stdout);
        let mut line = String::new();

        let res_meta: Response = match stdout.read_line(&mut line).await {
            Ok(0) => Response::default(),
            Ok(_) => serde_json::from_str(&line).unwrap(),
            Err(e) => panic!("{:?}", e),
        };

        let mut res = hyper::Response::builder().status(res_meta.status.unwrap_or(200));
        {
            let res_headers = res.headers_mut().unwrap();
            if let Some(headers) = res_meta.headers {
                for (key, value) in headers {
                    res_headers.insert(
                        http::header::HeaderName::from_bytes(key.as_bytes()).unwrap(),
                        http::header::HeaderValue::from_bytes(value.as_bytes()).unwrap(),
                    );
                }
            }

            if !res_headers.contains_key("content-type") {
                res_headers.insert("content-type", "text/plain".parse().unwrap());
            }
        }

        // todo: this should not return until the body stream has ended
        let stdout = tokio_util::io::ReaderStream::new(stdout);
        let stdout = hyper::Body::wrap_stream(stdout);
        res.body(stdout).expect("streaming response body")
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

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn handler_get() {
        let req = hyper::Request::get("https://api.cross.stream/")
            .body(hyper::Body::empty())
            .unwrap();
        let resp = handler(
            req,
            &"bash".into(),
            &vec!["-c".into(), r#"echo '{}'; jq .method"#.into()],
        )
        .await;
        assert_eq!(resp.status(), hyper::StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "text/plain");
        let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert_eq!(
            body,
            indoc! {r#"
            "GET"
            "#}
        );
    }

    #[tokio::test]
    async fn handler_post() {
        let req = hyper::Request::post("https://api.cross.stream/")
            .header("Last-Event-ID", 5)
            .body("zebody".into())
            .unwrap();
        let resp = handler(
            req,
            &"bash".into(),
            &vec!["-c".into(), r#"echo '{}'; tail -n1"#.into()],
        )
        .await;
        assert_eq!(resp.status(), hyper::StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "text/plain");
        let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert_eq!(
            body,
            indoc! {r#"
            zebody"#}
        );
    }

    #[tokio::test]
    async fn handler_response_empty() {
        let req = hyper::Request::get("https://api.cross.stream/")
            .body(hyper::Body::empty())
            .unwrap();
        let resp = handler(
            req,
            &"bash".into(),
            &vec![
                "-c".into(),
                r#"
                cat > /dev/null
                "#
                .into(),
            ],
        )
        .await;
        assert_eq!(resp.status(), hyper::StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "text/plain");
        let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert_eq!(body, "");
    }

    #[tokio::test]
    async fn handler_response_meta() {
        let req = hyper::Request::get("https://api.cross.stream/")
            .body(hyper::Body::empty())
            .unwrap();
        let resp = handler(
            req,
            &"bash".into(),
            &vec![
                "-c".into(),
                r#"
                echo '{"status":404,"headers":{"content-type":"text/markdown"}}'
                echo '# Header'
                "#
                .into(),
            ],
        )
        .await;
        assert_eq!(resp.status(), hyper::StatusCode::NOT_FOUND);
        assert_eq!(resp.headers().get("content-type").unwrap(), "text/markdown");
        let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert_eq!(
            body,
            indoc! {r#"
            # Header
            "#}
        );
    }
}
