use futures::TryStreamExt;
use hyper::service::{make_service_fn, service_fn};
use std::convert::Infallible;
use std::net::SocketAddr;
use tokio::signal::unix::{signal, SignalKind};

async fn hello_world(
    req: hyper::Request<hyper::Body>,
) -> Result<hyper::Response<hyper::Body>, Infallible> {
    let mut response = hyper::Response::new(hyper::Body::empty());

    match (req.method(), req.uri().path()) {
        (&hyper::Method::GET, "/") => {
            *response.body_mut() = hyper::Body::from("Try POSTing data to /echo");
        }

        (&hyper::Method::POST, "/echo") => {
            *response.body_mut() = req.into_body();
        }

        // Yet another route inside our match block...
        (&hyper::Method::POST, "/echo/uppercase") => {
            // This is actually a new `futures::Stream`...
            let mapping = req.into_body().map_ok(|chunk| {
                chunk
                    .iter()
                    .map(|byte| byte.to_ascii_uppercase())
                    .collect::<Vec<u8>>()
            });

            // Use `Body::wrap_stream` to convert it to a `Body`...
            *response.body_mut() = hyper::Body::wrap_stream(mapping);
        }

        _ => {
            *response.status_mut() = hyper::StatusCode::NOT_FOUND;
        }
    };

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
        // service_fn converts our function into a `Service`
        Ok::<_, Infallible>(service_fn(hello_world))
    });

    let server = hyper::Server::bind(&addr).serve(make_svc);

    let graceful = server.with_graceful_shutdown(shutdown_signal());

    if let Err(e) = graceful.await {
        eprintln!("server error: {}", e);
    }
}
