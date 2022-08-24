use futures_util::{SinkExt, StreamExt};

use tokio::io::AsyncWriteExt;
use tokio::io::{AsyncBufReadExt, BufReader};

use clap::{Args, Parser};
use serde::{Deserialize, Serialize};

use warp::Filter;

/*
 * todo:
 * - rework HTTP to take uri + headers as an optional command line arg
 *  - and optionally as fd:4
 *  - then the request body can be streamed to stdin
 * - respond with status code and headers via fd:5?
 *  - then stdout can stream to the response body
 */

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
#[clap(propagate_version = true)]
struct Cli {
    #[clap(
        short,
        long,
        value_parser,
        multiple_values = true,
        allow_hyphen_values = true,
        value_terminator = ";",
        value_name = "COMMAND"
    )]
    // TODO: how to get clap to take this as a <COMMAND> [ARGS]...??
    websocket: Option<Vec<String>>,

    #[clap(value_parser)]
    command: String,
    #[clap(value_parser)]
    args: Vec<String>,
}

#[derive(Args, Debug)]
struct CommandLine {
    #[clap(value_parser)]
    command: String,
    #[clap(value_parser)]
    args: Vec<String>,
}

#[tokio::main]
async fn main() {
    let args = Cli::parse();
    serve(args.command, args.args, args.websocket, 3030).await
}

async fn serve(command: String, args: Vec<String>, websocket: Option<Vec<String>>, port: u16) {
    fn with_command(
        command: String,
    ) -> impl Filter<Extract = (String,), Error = std::convert::Infallible> + Clone {
        warp::any().map(move || command.clone())
    }

    fn with_args(
        args: Vec<String>,
    ) -> impl Filter<Extract = (Vec<String>,), Error = std::convert::Infallible> + Clone {
        warp::any().map(move || args.clone())
    }

    let base = warp::method()
        .and(warp::path::full())
        .and(warp::header::headers_cloned());

    let route_http = base
        .clone()
        .and(with_command(command))
        .and(with_args(args))
        .and(warp::body::bytes())
        .and_then(handle_http);

    if let Some(mut args) = websocket {
        let command = args.remove(0);
        let route_ws = base
            .clone()
            .and(with_command(command))
            .and(with_args(args))
            .and(warp::ws())
            .and_then(handle_ws);
        // TODO: I can't get rust to allow assigning this to a variable and then making a single
        // call to warp::serve
        warp::serve(route_ws.or(route_http))
            .run(([127, 0, 0, 1], port))
            .await;
        return;
    };

    warp::serve(route_http).run(([127, 0, 0, 1], port)).await;
}

pub async fn handle_ws(
    method: http::method::Method,
    path: warp::filters::path::FullPath,
    headers: http::header::HeaderMap,
    command: String,
    args: Vec<String>,
    ws: warp::ws::Ws,
) -> Result<impl warp::Reply, std::convert::Infallible> {
    Ok(ws.on_upgrade(move |websocket| async {
        let mut p = tokio::process::Command::new(command)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // todo: stderr
            .spawn()
            .expect("failed to spawn");

        let (mut tx, mut rx) = websocket.split();

        // relay websocket messages to the child processes stdin
        let mut stdin = p.stdin.take().unwrap();
        tokio::spawn(async move {
            while let Some(message) = rx.next().await {
                let message = format!(
                    "stdin: {}\n",
                    message
                        .expect("todo: handle different new types?")
                        .to_str()
                        .unwrap()
                );
                stdin
                    .write_all(message.as_bytes())
                    .await
                    .expect("what should happen when the child processes stdin closes?");
            }
        });

        // relay the child processes stdout to the websocket
        let stdout = p.stdout.take().unwrap();
        let buf = BufReader::new(stdout);
        let mut lines = buf.lines();

        while let Some(line) = lines.next_line().await.expect("todo") {
            tx.send(warp::ws::Message::text(line))
                .await
                .expect("moar todo");
        }

        let status = p.wait().await.expect("command wasn't running");
        println!("peace: {:>}", status);
    }))
}

pub async fn handle_http(
    method: http::method::Method,
    path: warp::filters::path::FullPath,
    headers: http::header::HeaderMap,
    command: String,
    args: Vec<String>,
    body: warp::hyper::body::Bytes,
) -> Result<impl warp::Reply, std::convert::Infallible> {
    #[derive(Serialize, Deserialize)]
    struct Request {
        #[serde(with = "http_serde::method")]
        method: http::method::Method,
        #[serde(with = "http_serde::header_map")]
        headers: http::header::HeaderMap,
        #[serde(with = "http_serde::uri")]
        path: http::Uri,
        body: String,
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct Response {
        status: Option<u16>,
        headers: Option<std::collections::HashMap<String, String>>,
        body: String,
    }

    let request = serde_json::json!(Request {
        method: method,
        path: path.as_str().parse().unwrap(),
        headers: headers,
        body: String::from_utf8(body.to_vec()).unwrap(),
    });

    let res = process(command, args, request.to_string().as_bytes()).await;
    let res: Response = serde_json::from_slice(&res.stdout).unwrap();

    let mut builder = http::Response::builder()
        .status(res.status.unwrap_or(200))
        .header("Content-Type", "text/html; charset=utf8");

    if let Some(src_headers) = res.headers {
        let headers = builder.headers_mut().unwrap();
        for (key, value) in src_headers.iter() {
            headers.insert(
                http::header::HeaderName::try_from(key.clone()).unwrap(),
                http::header::HeaderValue::try_from(value.clone()).unwrap(),
            );
        }
    }

    Ok(builder.body(res.body).unwrap())
}

async fn process(command: String, args: Vec<String>, i: &[u8]) -> std::process::Output {
    let mut p = tokio::process::Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn");

    {
        let mut stdin = p.stdin.take().unwrap();
        stdin.write_all(i).await.unwrap();
    }

    let res = p.wait_with_output().await.expect("todo");
    assert_eq!(res.status.code().unwrap(), 0);
    res
}

#[tokio::test]
async fn test_process() {
    assert_eq!(
        process("cat".to_string(), vec![], b"foo").await.stdout,
        b"foo"
    );
}

#[tokio::test]
async fn test_serve_defaults() {
    tokio::spawn(serve(
        "echo".to_string(),
        vec![r#"{"body": "hai"}"#.to_string()],
        None,
        3030,
    ));
    // give the server a chance to start
    tokio::time::sleep(std::time::Duration::from_millis(1)).await;

    let resp = reqwest::get("http://127.0.0.1:3030/").await.unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "text/html; charset=utf8",
    );
    assert_eq!(resp.text().await.unwrap(), "hai");
}

#[tokio::test]
async fn test_serve_override() {
    tokio::spawn(serve(
        "echo".to_string(),
        vec![r#"{
            "body": "sorry",
            "status": 404,
            "headers": {
                "content-type": "text/plain"
            }
        }"#
        .to_string()],
        None,
        3031,
    ));
    // give the server a chance to start
    tokio::time::sleep(std::time::Duration::from_millis(1)).await;

    let resp = reqwest::get("http://127.0.0.1:3031/").await.unwrap();

    assert_eq!(resp.status(), 404);
    assert_eq!(resp.headers().get("content-type").unwrap(), "text/plain");
    assert_eq!(resp.text().await.unwrap(), "sorry");
}
