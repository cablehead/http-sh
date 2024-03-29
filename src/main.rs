use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use std::path::PathBuf;

use futures::TryStreamExt as _;

use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;

use serde_json::json;

use clap::Parser;

use command_fds::tokio::CommandFdAsyncExt;
use command_fds::FdMapping;

mod listener;
use http_sh::{Request, Response};

#[derive(Parser, Debug, Clone)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Path to files to serve statically
    #[clap(short, long, value_parser)]
    static_path: Option<PathBuf>,

    /// Path to a PEM-encoded file with your TLS private key and certificates. When provided, the
    /// server will use HTTPS, otherwise HTTP
    #[clap(short, long, value_parser, value_name = "PEM_FILE")]
    tls: Option<PathBuf>,

    /// Address to listen on [HOST]:PORT or <PATH> for Unix domain socket
    #[clap(value_parser, value_name = "LISTEN_ADDR")]
    listen: String,

    #[clap(value_parser)]
    command: String,
    #[clap(value_parser)]
    args: Vec<String>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let accept_tls = args.tls.clone().map(configure_tls);

    let mut server = listener::Listener::bind(&args.listen).await.unwrap();

    let is_unix = match server {
        listener::Listener::Tcp(_) => false,
        listener::Listener::Unix(_) => true,
    };

    /*
    use tokio::signal::unix::{signal, SignalKind};

    async fn shutdown_signal() {
        let mut sigint = signal(SignalKind::interrupt()).unwrap();
        let mut sigterm = signal(SignalKind::terminate()).unwrap();
        tokio::select! {
            _ = sigint.recv() => {},
            _ = sigterm.recv() => {},
        }
    }

    tokio::select! {
        result = listener.accept() => {
            let (stream, remote_addr) = result.unwrap();
            println!("Got connection");
        },
        _ = shutdown_signal() => {
            println!("Received shutdown signal. Shutting down gracefully...");
        },
    }
    */

    println!(
        "{}",
        json!({"stamp": scru128::new(), "message": "start", "address": format!("{}", server)})
    );

    loop {
        let args = args.clone();
        let accept_tls = accept_tls.clone();

        let (stream, remote_addr) = server.accept().await.unwrap();

        let stream = if let Some(acceptor) = accept_tls {
            Box::new(acceptor.accept(stream).await.unwrap())
        } else {
            stream
        };

        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let svc_fn = hyper::service::service_fn(move |req| {
            let args = args.clone();
            let shutdown_rx = shutdown_rx.clone();
            async move {
                Ok::<hyper::Response<hyper::Body>, Infallible>(
                    handler(
                        shutdown_rx,
                        req,
                        remote_addr,
                        &args.static_path,
                        &args.command,
                        &args.args,
                    )
                    .await,
                )
            }
        });

        tokio::task::spawn(async move {
            match hyper::server::conn::Http::new()
                .serve_connection(stream, svc_fn)
                .await
            {
                Ok(_) => (),
                Err(e) => {
                    if e.is_incomplete_message() {
                        return;
                    }
                    if !is_unix {
                        panic!("unexpected error: {:?}", e)
                    }

                    // hyper returns an error attempting to `Shutdown` a unix domain socket
                    // connection
                    // https://github.com/hyperium/hyper/blob/master/src/error.rs#L49-L51
                    let e: std::io::Error = *e.into_cause().unwrap().downcast().unwrap();
                    match e.kind() {
                        std::io::ErrorKind::NotConnected => return,
                        _ => panic!("unexpected error: {:?}", e),
                    }
                }
            };
            drop(shutdown_tx);
        });
    }
}

async fn handler(
    mut shutdown_rx: watch::Receiver<bool>,
    req: hyper::Request<hyper::Body>,
    addr: Option<SocketAddr>,
    static_path: &Option<PathBuf>,
    command: &String,
    args: &Vec<String>,
) -> hyper::Response<hyper::Body> {
    if let Some(static_path) = static_path {
        let resolved = hyper_staticfile::resolve(&static_path, &req).await.unwrap();
        if let hyper_staticfile::ResolveResult::Found(_, _, _) = resolved {
            return hyper_staticfile::ResponseBuilder::new()
                .request(&req)
                .build(resolved)
                .unwrap();
        }
    }

    let (req_reader, mut req_writer) = tokio_pipe::pipe().unwrap();
    let (mut res_reader, res_writer) = tokio_pipe::pipe().unwrap();

    let mut p = tokio::process::Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .fd_mappings(vec![
            FdMapping {
                parent_fd: req_reader.as_raw_fd(),
                child_fd: 3,
            },
            FdMapping {
                parent_fd: res_writer.as_raw_fd(),
                child_fd: 4,
            },
        ])
        .unwrap()
        .spawn()
        .expect("failed to spawn");

    drop(req_reader);
    drop(res_writer);

    let (req_parts, req_body) = req.into_parts();

    let uri = req_parts.uri.clone().into_parts();

    let authority: Option<String> = uri.authority.as_ref().map(|a| a.to_string()).or_else(|| {
        req_parts
            .headers
            .get("host")
            .map(|a| a.to_str().unwrap().to_owned())
    });

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

    let mut req_meta = Request {
        stamp: scru128::new(),
        message: "request".to_string(),
        proto: format!("{:?}", req_parts.version),
        method: req_parts.method,
        authority,
        remote_ip: addr.as_ref().map(|a| a.ip()),
        remote_port: addr.as_ref().map(|a| a.port()),
        headers: req_parts.headers,
        uri: req_parts.uri,
        path,
        query,
        response: None,
    };

    let req_json = serde_json::to_string(&req_meta).unwrap();
    let mut stdin = p.stdin.take().expect("failed to take stdin");

    tokio::spawn(async move {
        req_writer
            .write_all(format!("{}\n", &req_json).as_bytes())
            .await
            .or_else(|e| match e.kind() {
                // ignore BrokenPipe errors, as the child process may have exited without
                // reading the metadata
                std::io::ErrorKind::BrokenPipe => Ok(()),
                _ => Err(e),
            })
            .expect("failed to write request metadata");
        drop(req_writer);

        let req_body = req_body.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e));
        let mut req_body = tokio_util::io::StreamReader::new(req_body);
        tokio::io::copy(&mut req_body, &mut stdin)
            .await
            .expect("streaming request body to stdin");
    });

    let mut buf = String::new();
    res_reader.read_to_string(&mut buf).await.unwrap();
    drop(res_reader);

    let mut res_meta = if buf.is_empty() {
        Response::default()
    } else {
        serde_json::from_str::<Response>(&buf).unwrap()
    };

    req_meta.response = Some(res_meta.clone());

    let status = res_meta.status.unwrap_or(200);
    res_meta.status = Some(status);

    let mut res = hyper::Response::builder().status(status);
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

    let (mut sender, body) = hyper::Body::channel();
    let stdout = p.stdout.take().expect("failed to take stdout");
    tokio::spawn(async move {
        let mut stdout = tokio::io::BufReader::new(stdout);
        let mut buf = [0; 4096];

        loop {
            tokio::select! {
                Ok(n) = stdout.read(&mut buf[..]) => {
                    if n == 0 {
                        break; // EOF reached
                    }

                    if sender.send_data(buf[..n].to_vec().into()).await.is_err() {
                        break;
                    }
                }
                status = shutdown_rx.changed() => {
                    if status.is_err() {
                        break;
                    }
                    if *shutdown_rx.borrow() {
                        break;
                    }
                },
            }
        }

        let pid = nix::unistd::Pid::from_raw(p.id().unwrap() as i32);
        nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM).unwrap();
    });

    let ret = res.body(body).unwrap();
    println!("{}", serde_json::to_string(&req_meta).unwrap());
    ret
}

fn configure_tls(pem: PathBuf) -> tokio_rustls::TlsAcceptor {
    let pem = std::fs::File::open(pem).unwrap();
    let mut pem = std::io::BufReader::new(pem);

    let items = rustls_pemfile::read_all(&mut pem).unwrap();

    let certs: Vec<rustls::Certificate> = items
        .iter()
        .filter_map(|item| match item {
            rustls_pemfile::Item::X509Certificate(cert) => Some(rustls::Certificate(cert.to_vec())),
            _ => None,
        })
        .collect();

    let key = items
        .into_iter()
        .find_map(|item| match item {
            rustls_pemfile::Item::RSAKey(key) => Some(rustls::PrivateKey(key)),
            rustls_pemfile::Item::PKCS8Key(key) => Some(rustls::PrivateKey(key)),
            rustls_pemfile::Item::ECKey(key) => Some(rustls::PrivateKey(key)),
            rustls_pemfile::Item::X509Certificate(_) => None,
            _ => todo!(),
        })
        .unwrap();

    let mut config = rustls::ServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn handler_get() {
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let req = hyper::Request::get("https://api.cross.stream/")
            .body(hyper::Body::empty())
            .unwrap();
        let resp = handler(
            rx,
            req,
            None,
            &None,
            &"echo".into(),
            &vec!["hello world".into()],
        )
        .await;
        assert_eq!(resp.status(), hyper::StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "text/plain");
        let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert_eq!(body, "hello world\n");
    }

    #[tokio::test]
    async fn handler_post() {
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let req = hyper::Request::post("https://api.cross.stream/")
            .header("Last-Event-ID", 5)
            .body("zebody".into())
            .unwrap();
        let resp = handler(rx, req, None, &None, &"cat".into(), &vec![]).await;
        assert_eq!(resp.status(), hyper::StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "text/plain");
        let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert_eq!(body, "zebody");
    }

    #[tokio::test]
    async fn handler_response_empty() {
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let req = hyper::Request::get("https://api.cross.stream/")
            .body(hyper::Body::empty())
            .unwrap();
        let resp = handler(
            rx,
            req,
            None,
            &None,
            &"sh".into(),
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
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let req = hyper::Request::get("https://api.cross.stream/notfound")
            .body(hyper::Body::empty())
            .unwrap();
        let resp = handler(
            rx,
            req,
            None,
            &None,
            &"sh".into(),
            &vec![
                "-c".into(),
                r#"
                echo '{"status":404,"headers":{"content-type":"text/markdown"}}' >&4
                echo '# Not Found'
                jq -r .path <&3
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
            # Not Found
            /notfound
            "#}
        );
    }

    #[tokio::test]
    async fn handler_static() {
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let d = tempfile::tempdir().unwrap();

        let subdir = d.path().join("static");
        std::fs::create_dir(&subdir).unwrap();
        let filename = subdir.join("index.html");
        std::fs::write(&filename, "hello world").unwrap();

        let static_path = Some(d.into_path());

        // static file exists
        let req = hyper::Request::get("https://api.cross.stream/static/")
            .body(hyper::Body::empty())
            .unwrap();
        let resp = handler(
            rx.clone(),
            req,
            None,
            &static_path,
            &"echo".into(),
            &vec!["hello world".into()],
        )
        .await;
        assert_eq!(resp.status(), hyper::StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "text/html");
        let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert_eq!(
            body,
            indoc! {r#"
            hello world"#}
        );

        // static file not found, fall back to command + args
        let req = hyper::Request::get("https://api.cross.stream/")
            .body(hyper::Body::empty())
            .unwrap();
        let resp = handler(
            rx.clone(),
            req,
            None,
            &static_path,
            &"echo".into(),
            &vec!["hello world".into()],
        )
        .await;
        assert_eq!(resp.status(), hyper::StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "text/plain");
        let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert_eq!(body, "hello world\n");
    }
}
