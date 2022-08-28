use std::convert::Infallible;
use std::net::SocketAddr;

use futures::TryStreamExt;
use hyper::service::{make_service_fn, service_fn};
use tokio::signal::unix::{signal, SignalKind};

async fn hello_world(
    req: hyper::Request<hyper::Body>,
) -> Result<hyper::Response<hyper::Body>, Infallible> {
    let mut p = tokio::process::Command::new("./run.sh")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn");

    let body = hyper::Body::wrap_stream(tokio_util::io::ReaderStream::new(p.stdout.unwrap()));
    let response = hyper::Response::new(body);
    Ok(response)
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
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    let make_svc = make_service_fn(|_conn| async {
        Ok::<_, Infallible>(service_fn(hello_world))
    });
    let server = hyper::Server::bind(&addr).serve(make_svc);
    let graceful = server.with_graceful_shutdown(shutdown_signal());
    if let Err(e) = graceful.await {
        eprintln!("server error: {}", e);
    }
}
