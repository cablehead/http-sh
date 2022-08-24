use futures_util::{SinkExt, StreamExt};

use tokio::io::AsyncWriteExt;
use tokio::io::{AsyncBufReadExt, BufReader};

use clap::{Args, Parser};

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
        // todo: I can't work out how to use body::stream. kinda wish warp just gave you a
        // hyper::body::Body
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
    _method: http::method::Method,
    _path: warp::filters::path::FullPath,
    _headers: http::header::HeaderMap,
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
    _method: http::method::Method,
    _path: warp::filters::path::FullPath,
    _headers: http::header::HeaderMap,
    command: String,
    args: Vec<String>,
    body: warp::hyper::body::Bytes,
) -> Result<impl warp::Reply, std::convert::Infallible> {
    let mut p = tokio::process::Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn");

    {
        let mut stdin = p.stdin.take().unwrap();
        stdin.write_all(&body).await.unwrap();
    }

    let res = p.wait_with_output().await.expect("todo");
    assert_eq!(res.status.code().unwrap(), 0);
    // todo: stream child stdout to http response body
    let builder = http::Response::builder()
        .status(200)
        .header("Content-Type", "text/html; charset=utf8");
    Ok(builder.body(res.stdout))
}

#[tokio::test]
async fn test_serve_defaults() {
    tokio::spawn(serve("cat".to_string(), vec![], None, 3030));
    // give the server a chance to start
    tokio::time::sleep(std::time::Duration::from_millis(1)).await;

    let resp = reqwest::Client::new()
        .post("http://127.0.0.1:3030/")
        .body("oh hai")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "text/html; charset=utf8",
    );
    assert_eq!(resp.text().await.unwrap(), "oh hai");
}

/*
 * todo: bring back the ability to set meta fields in the response
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
*/
