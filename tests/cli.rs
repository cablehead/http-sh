use std::io::BufRead;
use std::io::Read;

use std::process::Command;

// Taken from:
// https://github.com/assert-rs/assert_cmd/blob/e71a9f7b15596dd2aeea911bedbbd1859d84fa67/src/cargo.rs#L183-L208
fn target_dir() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .map(|mut path| {
            path.pop();
            if path.ends_with("deps") {
                path.pop();
            }
            path
        })
        .unwrap()
}

fn cargo_bin(name: &str) -> std::path::PathBuf {
    let env_var = format!("CARGO_BIN_EXE_{}", name);
    std::env::var_os(&env_var)
        .map(|p| p.into())
        .unwrap_or_else(|| target_dir().join(format!("{}{}", name, std::env::consts::EXE_SUFFIX)))
}

fn run_curl(args: Vec<&str>) -> std::process::Output {
    let mut command = Command::new("curl");
    command.arg("-s");
    for arg in args {
        command.arg(arg);
    }
    command.output().unwrap()
}

#[test]
fn serve_unix() {
    let temp_dir = tempfile::tempdir().unwrap();
    let path = temp_dir.path().join("test.sock");
    let path = path.to_str().unwrap();

    let http_sh = cargo_bin("http-sh");
    let want = "Hello from server!";

    let mut serve = Command::new(http_sh)
        .arg(path)
        .arg("--")
        .arg("printf")
        .arg(want)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // read startup log line to ensure serve is ready
    let stdout = serve.stdout.take().unwrap();
    let mut stdout = std::io::BufReader::new(stdout);
    let mut read = String::new();
    stdout.read_line(&mut read).unwrap();
    read.clear();

    let got = run_curl(vec![
        "--http1.1",
        "--unix-socket",
        path,
        "http://localhost/",
    ]);
    assert_eq!(want.as_bytes(), got.stdout);

    // next , parse got.stdout to a Response and assert HOST header
    let log: http_sh::Request = serde_json::from_slice(&got.stdout).unwrap();
    println!("{:?}", log);

    let got = run_curl(vec![
        "--http2-prior-knowledge",
        "--unix-socket",
        path,
        "http://localhost/",
    ]);
    println!("got: {:?}", got);

    serve.kill().unwrap();
    // todo: graceful shutdown
    let _exit_status = serve.wait().unwrap();
    // println!("exit_status: {:?}", exit_status);

    // read remaining logs
    stdout.read_to_string(&mut read).unwrap();
    println!("remaining logs:\n{}", read);

    let mut stderr = String::new();
    let n = serve
        .stderr
        .as_mut()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert_eq!(0, n, "stderr is not empty: \n---\n{}---\n", stderr);
}

#[test]
fn serve_tcp() {
    let http_sh = cargo_bin("http-sh");
    let want = "Hello from server!";

    let mut serve = Command::new(http_sh)
        .arg(":0")
        .arg("--")
        .arg("printf")
        .arg(want)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // read startup log line to ensure serve is ready
    let stdout = serve.stdout.take().unwrap();
    let mut stdout = std::io::BufReader::new(stdout);
    let mut read = String::new();
    stdout.read_line(&mut read).unwrap();
    let log: serde_json::Value = serde_json::from_str(&read).unwrap();

    let got = Command::new("curl")
        .arg("-s")
        .arg(log["address"].as_str().unwrap())
        .output()
        .unwrap();
    assert_eq!(want.as_bytes(), got.stdout);

    serve.kill().unwrap();
    // todo: graceful shutdown
    let _exit_status = serve.wait().unwrap();
    // println!("exit_status: {:?}", exit_status);

    // read remaining logs
    stdout.read_to_string(&mut read).unwrap();
    // println!("remaining logs: {}", read);

    let mut stderr = String::new();
    let n = serve
        .stderr
        .as_mut()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert_eq!(0, n, "stderr is not empty: \n---\n{}---\n", stderr);
}
