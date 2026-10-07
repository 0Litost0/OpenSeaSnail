use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Environment {
    child: Option<Child>,
    home: tempfile::TempDir,
    base: String,
    pids: Vec<u32>,
}
impl Environment {
    fn new() -> Self {
        let mut env = Self {
            child: None,
            home: tempfile::tempdir().unwrap(),
            base: String::new(),
            pids: vec![],
        };
        env.start();
        env
    }
    fn start(&mut self) {
        let child = Command::new(env!("CARGO_BIN_EXE_seasnail-poc-host"))
            .env("POC_DATA_DIR", self.home.path())
            .env("POC_RUNTIME", "mock")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        self.pids.push(child.id());
        self.child = Some(child);
        let stdout = self.child.as_mut().unwrap().stdout.take().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
            let _ = sender.send(result);
        });
        let line = receiver
            .recv_timeout(Duration::from_secs(15))
            .expect("host ready deadline")
            .unwrap();
        let ready: Value = serde_json::from_str(&line).expect("host ready JSON");
        self.base = format!("http://127.0.0.1:{}/api/v1", ready["port"]);
    }
    fn stop(&mut self) -> bool {
        let Some(mut child) = self.child.take() else {
            return true;
        };
        drop(child.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status.success();
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Environment {
    fn drop(&mut self) {
        let clean = self.stop();
        eprintln!("POC cleanup: graceful={clean}, host_pids={:?}", self.pids);
        if !clean && !std::thread::panicking() {
            panic!("host required forced cleanup");
        }
    }
}

#[test]
fn dict_001_crud_restart() {
    let mut env = Environment::new();
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    eprintln!("STEP 创建账户");
    let password = std::env::var("POC_PASSWORD").unwrap_or_else(|_| {
        format!(
            "poc-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        )
    });
    let setup = client
        .post(format!("{}/auth/setup", env.base))
        .json(&json!({"username":"poc","password":password}))
        .send()
        .unwrap();
    assert_eq!(setup.status().as_u16(), 201);
    let token = setup.json::<Value>().unwrap()["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    let call =
        |base: &str, method: &str, path: &str, data: Option<Value>, expected: u16| -> Value {
            let mut req = client
                .request(method.parse().unwrap(), format!("{base}{path}"))
                .bearer_auth(&token);
            if let Some(data) = data {
                req = req.json(&data);
            }
            let res = req.send().unwrap();
            assert_eq!(res.status().as_u16(), expected, "HTTP {method} {path}");
            if expected == 204 {
                Value::Null
            } else {
                res.json().unwrap()
            }
        };
    eprintln!("STEP 词典增改删查");
    let added = call(
        &env.base,
        "POST",
        "/dictionary/entries",
        Some(json!({"terms":["SeaSnail", "discard"]})),
        200,
    );
    let entries = added["added"].as_array().unwrap();
    let id = entries.iter().find(|v| v["term"] == "SeaSnail").unwrap()["id"]
        .as_str()
        .unwrap();
    let discard = entries.iter().find(|v| v["term"] == "discard").unwrap()["id"]
        .as_str()
        .unwrap();
    call(
        &env.base,
        "PUT",
        &format!("/dictionary/entries/{id}"),
        Some(json!({"term":"SeaSnail v2"})),
        200,
    );
    call(
        &env.base,
        "DELETE",
        &format!("/dictionary/entries/{discard}"),
        None,
        204,
    );
    let before = call(&env.base, "GET", "/dictionary", None, 200);
    assert_eq!(before["items"][0]["term"], "SeaSnail v2");
    assert_eq!(before["items"].as_array().unwrap().len(), 1);
    eprintln!("STEP 真正重启服务");
    assert!(env.stop(), "old host must exit gracefully");
    env.start();
    assert_ne!(env.pids[0], env.pids[1]);
    eprintln!("STEP 重启后读取");
    let after = call(&env.base, "GET", "/dictionary", None, 200);
    let actual: Vec<&str> = after["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["term"].as_str().unwrap())
        .collect();
    let expected = if std::env::var("POC_FAULT").as_deref() == Ok("1") {
        "INJECTED_WRONG_EXPECTATION"
    } else {
        "SeaSnail v2"
    };
    assert_eq!(actual, vec![expected], "DICT-001 / 重启后读取");
}
