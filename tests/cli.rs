use std::io::BufRead;
use std::io::Read;
use std::str::FromStr;

use std::process::Command;

use sysinfo::SystemExt;

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
fn connection_cleanup() {
    let temp_dir = tempfile::tempdir().unwrap();
    let path = temp_dir.path().join("test.sock");
    let path = path.to_str().unwrap();

    let http_sh = cargo_bin("http-sh");

    let serve = Command::new(http_sh)
        .arg(path)
        .arg("--")
        .arg("bash")
        .arg("-c")
        .arg(
            r#"
            exec 4>&-
            sleep 3600 &
            SLEEPID=$!
            echo $SLEEPID
            trap 'kill $SLEEPID' SIGTERM
            wait
            "#,
        )
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let mut serve = scopeguard::guard(serve, |mut serve| {
        let _ = serve.kill();
    });

    // read startup log line to ensure serve is ready
    let stdout = serve.stdout.take().unwrap();
    let stdout = std::io::BufReader::new(stdout);
    let mut loglines = stdout.lines();
    println!("logline: {:?}", loglines.next().unwrap());

    let mut curl = Command::new("curl")
        .arg("-s")
        .arg("--no-buffer")
        .arg("--unix-socket")
        .arg(path)
        .arg("http://localhost/")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let curl_out = curl.stdout.take().unwrap();
    let curl_out = std::io::BufReader::new(curl_out);
    let mut curl_lines = curl_out.lines();

    let pid = curl_lines.next().unwrap().unwrap();
    let pid = sysinfo::Pid::from_str(&pid).unwrap();

    let mut sys = sysinfo::System::new_all();
    sys.refresh_all();
    assert!(sys.process(pid).is_some());

    let _ = curl.kill().unwrap();
    let _ = curl.wait().unwrap();

    sys.refresh_all();
    assert!(sys.process(pid).is_none());
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
    let stdout = std::io::BufReader::new(stdout);
    let mut loglines = stdout.lines();

    let _logline = loglines.next().unwrap().unwrap();
    // todo: assert start logline looks OK

    let got = run_curl(vec![
        "--http1.1",
        "--unix-socket",
        path,
        "http://localhost:5555/",
    ]);
    assert_eq!(want.as_bytes(), got.stdout);

    // next , parse got.stdout to a Response and assert HOST header
    let logline = loglines.next().unwrap().unwrap();
    let log: http_sh::Request = serde_json::from_str(&logline).unwrap();
    assert_eq!(log.proto, "HTTP/1.1");
    assert_eq!(log.authority, Some("localhost:5555".to_string()));

    let got = run_curl(vec![
        "--http2-prior-knowledge",
        "--unix-socket",
        path,
        "http://localhost:5555/",
    ]);
    assert_eq!(want.as_bytes(), got.stdout);
    let logline = loglines.next().unwrap().unwrap();
    assert_eq!(log.authority, Some("localhost:5555".to_string()));

    serve.kill().unwrap();
    // todo: graceful shutdown
    let _exit_status = serve.wait().unwrap();
    // println!("exit_status: {:?}", exit_status);

    let log: http_sh::Request = serde_json::from_str(&logline).unwrap();
    assert_eq!(log.proto, "HTTP/2.0");

    for line in loglines {
        println!("remaining stdout: {}", line.unwrap());
    }

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
