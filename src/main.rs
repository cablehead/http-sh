use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use std::path::PathBuf;

use futures::TryStreamExt as _;

use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

use serde::{json, Deserialize, Serialize};

use clap::Parser;

use command_fds::tokio::CommandFdAsyncExt;
use command_fds::FdMapping;

mod listener;

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

    let mut listener = listener::Listener::bind(&args.listen).await.unwrap();

    println!(
        "{}",
        json!({"stamp": scru128::new(), "message": "start", "address": format!("{}", listener)})
    );

    loop {
        let args = args.clone();
        let accept_tls = accept_tls.clone();

        let (tcp_stream, remote_addr) = listener.accept().await.unwrap();

        let svc_fn = hyper::service::service_fn(move |req| {
            let args = args.clone();
            async move {
                Ok::<hyper::Response<hyper::Body>, Infallible>(
                    handler(
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
            // todo: if I understood Rust better, itd be nice to avoid duplicating the call to
            // Http::new().serve_connection
            if let Some(acceptor) = accept_tls.clone() {
                hyper::server::conn::Http::new()
                    .serve_connection(acceptor.accept(tcp_stream).await.unwrap(), svc_fn)
                    .await
                    .unwrap();
            } else {
                hyper::server::conn::Http::new()
                    .serve_connection(tcp_stream, svc_fn)
                    .await
                    .unwrap();
            }
        });
    }

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

    let graceful = server.with_graceful_shutdown(shutdown_signal());
    if let Err(e) = graceful.await {
        eprintln!("server error: {e}");
    }
    */
}

async fn handler(
    req: hyper::Request<hyper::Body>,
    addr: Option<SocketAddr>,
    static_path: &Option<PathBuf>,
    command: &String,
    args: &Vec<String>,
) -> hyper::Response<hyper::Body> {
    #[derive(Serialize, Deserialize)]
    struct Request {
        request_id: scru128::Scru128Id,
        #[serde(with = "http_serde::method")]
        method: http::method::Method,
        proto: String,
        remote_ip: Option<std::net::IpAddr>,
        remote_port: Option<u16>,
        #[serde(with = "http_serde::header_map")]
        headers: http::header::HeaderMap,
        #[serde(with = "http_serde::uri")]
        uri: http::Uri,
        path: String,
        query: HashMap<String, String>,
    }

    #[derive(Default, Debug, Serialize, Deserialize)]
    struct Response {
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<scru128::Scru128Id>,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        headers: Option<std::collections::HashMap<String, String>>,
    }

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

    let request_id = scru128::new();

    let req_meta = json!(Request {
        request_id,
        proto: format!("{:?}", req_parts.version),
        remote_ip: addr.as_ref().map(|a| a.ip()),
        remote_port: addr.as_ref().map(|a| a.port()),
        method: req_parts.method,
        headers: req_parts.headers,
        uri: req_parts.uri,
        path,
        query,
    });

    println!("{}", json!({"app": "http.request", "detail": req_meta}));

    let write_stdin = async {
        req_writer
            .write_all(format!("{}\n", &req_meta.to_string()).as_bytes())
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
        let mut stdin = p.stdin.take().expect("failed to take stdin");
        tokio::io::copy(&mut req_body, &mut stdin)
            .await
            .expect("streaming request body");
    };

    let read_stdout = async {
        let mut buf = String::new();
        res_reader.read_to_string(&mut buf).await.unwrap();
        drop(res_reader);

        let mut res_meta = if buf.is_empty() {
            Response::default()
        } else {
            from_str::<Response>(&buf).unwrap()
        };

        let status = res_meta.status.unwrap_or(200);

        res_meta.request_id = Some(request_id);
        res_meta.status = Some(status);

        println!("{}", json!({"app": "http.response", "detail": res_meta}));

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

        let stdout = p.stdout.take().expect("failed to take stdout");
        let stdout = tokio::io::BufReader::new(stdout);
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
        let req = hyper::Request::get("https://api.cross.stream/")
            .body(hyper::Body::empty())
            .unwrap();
        let resp = handler(
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
        let req = hyper::Request::post("https://api.cross.stream/")
            .header("Last-Event-ID", 5)
            .body("zebody".into())
            .unwrap();
        let resp = handler(req, None, &None, &"cat".into(), &vec![]).await;
        assert_eq!(resp.status(), hyper::StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "text/plain");
        let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert_eq!(body, "zebody");
    }

    #[tokio::test]
    async fn handler_response_empty() {
        let req = hyper::Request::get("https://api.cross.stream/")
            .body(hyper::Body::empty())
            .unwrap();
        let resp = handler(
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
        let req = hyper::Request::get("https://api.cross.stream/notfound")
            .body(hyper::Body::empty())
            .unwrap();
        let resp = handler(
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
