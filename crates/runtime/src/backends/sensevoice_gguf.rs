//! SenseVoiceSmall + 锁定 GGUF server 的本地 sidecar driver。

use crate::contract::{has_visible_text, valid_visible_segment, OpenAiSegments, TranscribeReq};
use crate::{
    verify_legacy_gguf_artifact, ArtifactRequirements, Capabilities, ModelRuntime, RuntimeError,
    RuntimeKind, SidecarProcess,
};
use async_trait::async_trait;
use std::io;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::Mutex;

pub struct SenseVoiceGgufDriver {
    id: String,
    root: PathBuf,
    requirements: Option<ArtifactRequirements>,
    expected_size_bytes: Option<u64>,
    health_timeout: Duration,
    request_timeout: Duration,
    proc: Mutex<Option<Handle>>,
}
struct Handle {
    proc: SidecarProcess,
    port: u16,
}

impl SenseVoiceGgufDriver {
    #[cfg(test)]
    pub(crate) fn new(id: &str, root: PathBuf) -> Self {
        Self {
            id: id.into(),
            root,
            requirements: None,
            expected_size_bytes: None,
            health_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(300),
            proc: Mutex::new(None),
        }
    }

    pub(crate) fn new_verified(
        id: &str,
        root: PathBuf,
        requirements: ArtifactRequirements,
        expected_size_bytes: u64,
    ) -> Self {
        Self {
            id: id.into(),
            root,
            requirements: Some(requirements),
            expected_size_bytes: Some(expected_size_bytes),
            health_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(300),
            proc: Mutex::new(None),
        }
    }
}

#[async_trait]
impl ModelRuntime for SenseVoiceGgufDriver {
    fn id(&self) -> &str {
        &self.id
    }
    fn runtime_kind(&self) -> RuntimeKind {
        RuntimeKind::Gguf
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::sensevoice_gguf()
    }

    async fn start(&self, requested_port: u16) -> io::Result<()> {
        let mut guard = self.proc.lock().await;
        if guard.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "GGUF runtime already started; stop() first",
            ));
        }
        // 每次 spawn 前重新预检；发布目录不可变，开发目录须为 canonical 无 symlink 根。
        let root = self.root.clone();
        let runtime_id = self.id.clone();
        let requirements = self.requirements.clone();
        let expected_size_bytes = self.expected_size_bytes;
        let artifact = tokio::task::spawn_blocking(move || match requirements {
            Some(requirements) => super::factory::verify_gguf_against_requirements(
                &root,
                &runtime_id,
                &requirements,
                expected_size_bytes.expect("verified GGUF expected size"),
            ),
            None => verify_legacy_gguf_artifact(&root, &runtime_id)
                .map_err(super::factory::BackendRegistryError::from),
        })
        .await
        .map_err(|e| io::Error::other(format!("GGUF preflight task failed: {e}")))?
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        if artifact.identity.catalog_id != self.id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "GGUF artifact catalog_id does not match runtime id",
            ));
        }
        let server = artifact
            .file("sidecar")
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "GGUF sidecar role missing"))?
            .to_path_buf();
        let model = artifact
            .file("model")
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "GGUF model role missing"))?
            .to_path_buf();
        let vad = artifact
            .file("vad")
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "GGUF VAD role missing"))?
            .to_path_buf();
        let attempts = if requested_port == 0 { 5 } else { 1 };
        let mut last_error = io::ErrorKind::TimedOut;
        for _ in 0..attempts {
            // The server cannot inherit a pre-bound socket. Immediately verify its
            // listener PID after the unavoidable handover, retrying random ports on
            // contention and refusing to upload unless ownership is established.
            let port = if requested_port == 0 {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
                listener.local_addr()?.port()
            } else {
                requested_port
            };
            let mut command = tokio::process::Command::new(&server);
            command.args([
                "-m",
                &model.to_string_lossy(),
                "-vad",
                &vad.to_string_lossy(),
                "--max-audio-seconds",
                "3600",
                "127.0.0.1",
                &port.to_string(),
            ]);
            let mut proc = SidecarProcess::spawn(command, "sensevoice-gguf").await?;
            if !SidecarProcess::wait_health_at(port, "/health", self.health_timeout).await {
                let _ = proc.stop().await;
                last_error = io::ErrorKind::TimedOut;
                if requested_port != 0 {
                    break;
                }
                continue;
            }
            if !proc.owns_loopback_listener(port).await {
                let _ = proc.stop().await;
                last_error = io::ErrorKind::AddrInUse;
                if requested_port != 0 {
                    break;
                }
                continue;
            }
            *guard = Some(Handle { proc, port });
            return Ok(());
        }
        Err(io::Error::new(
            last_error,
            "GGUF server could not establish an exclusive loopback listener",
        ))
    }
    async fn stop(&self) -> io::Result<()> {
        if let Some(mut handle) = self.proc.lock().await.take() {
            handle.proc.stop().await?;
        }
        Ok(())
    }
    async fn health(&self) -> bool {
        match self.proc.lock().await.as_ref() {
            Some(handle) => {
                SidecarProcess::wait_health_at(handle.port, "/health", Duration::from_millis(500))
                    .await
            }
            None => false,
        }
    }
    async fn transcribe(&self, req: TranscribeReq) -> Result<OpenAiSegments, RuntimeError> {
        let (port, listener_owned) = match self.proc.lock().await.as_ref() {
            Some(handle) => (
                handle.port,
                handle.proc.owns_loopback_listener(handle.port).await,
            ),
            None => return Err(RuntimeError::NotStarted),
        };
        if !listener_owned {
            let _ = self.stop().await;
            return Err(RuntimeError::SidecarLost(
                "expected process no longer owns its loopback listener".into(),
            ));
        }
        let result = post_transcription(
            &format!("http://127.0.0.1:{port}/v1/audio/transcriptions"),
            &req,
            &self.id,
            self.request_timeout,
        )
        .await;
        if result.is_err() {
            let _ = self.stop().await;
        }
        result
    }
}

async fn post_transcription(
    url: &str,
    req: &TranscribeReq,
    model_id: &str,
    timeout: Duration,
) -> Result<OpenAiSegments, RuntimeError> {
    let wav = tokio::fs::read(&req.wav).await?;
    let form = reqwest::multipart::Form::new()
        .part(
            "file",
            reqwest::multipart::Part::bytes(wav)
                .file_name("audio.wav")
                .mime_str("audio/wav")
                .map_err(|e| RuntimeError::Protocol(e.to_string()))?,
        )
        .text("model", model_id.to_owned())
        .text("response_format", "verbose_json");
    let response = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(RuntimeError::Http)?
        .post(url)
        .multipart(form)
        .send()
        .await
        .map_err(RuntimeError::Http)?;
    if !response.status().is_success() {
        return Err(RuntimeError::BadStatus(response.status()));
    }
    let body = response.text().await.map_err(RuntimeError::Http)?;
    let mut result = serde_json::from_str::<OpenAiSegments>(&body)
        .map_err(|e| RuntimeError::Decode(e.to_string()))?;
    validate_response(&mut result)?;
    Ok(result)
}

fn validate_response(result: &mut OpenAiSegments) -> Result<(), RuntimeError> {
    let text_visible = has_visible_text(&result.text);
    if !text_visible && result.segments.is_empty() {
        // SenseVoice GGUF 不承诺可验证 words；空结果与非空结果均不透传词级证据。
        result.words.clear();
        return Ok(());
    }
    if !text_visible
        || result.segments.is_empty()
        || result
            .segments
            .iter()
            .any(|segment| !valid_visible_segment(segment))
    {
        return Err(RuntimeError::Protocol(
            "invalid verbose_json text or segment boundaries".into(),
        ));
    }
    result.words.clear();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn hash(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }
    fn fixture(server: &[u8]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let entries = [
            ("sensevoice-server", server),
            ("sensevoice.gguf", b"model".as_slice()),
            ("fsmn-vad.gguf", b"vad".as_slice()),
        ];
        for (name, bytes) in entries {
            fs::write(dir.path().join(name), bytes).unwrap();
        }
        #[cfg(unix)]
        fs::set_permissions(
            dir.path().join("sensevoice-server"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let files: Vec<_> = entries.iter().map(|(name, bytes)| serde_json::json!({"path":name,"sha256":hash(bytes),"size_bytes":bytes.len()})).collect();
        fs::write(dir.path().join("runtime-manifest.json"), serde_json::json!({"schema_version":1,"runtime":"gguf","model_id":"sensevoice-small","variant":"q8","model_file":"sensevoice.gguf","vad_file":"fsmn-vad.gguf","source_revision":"6991744856587fa44379e8b5dcc432debffeb1be","api_contract_version":1,"files":files}).to_string()).unwrap();
        dir
    }
    #[tokio::test]
    #[cfg(unix)]
    async fn start_health_duplicate_start_and_idempotent_stop() {
        let server = br##"#!/usr/bin/env perl
use IO::Socket::INET;
my $s = IO::Socket::INET->new(LocalAddr=>'127.0.0.1',LocalPort=>$ARGV[-1],Proto=>'tcp',Listen=>8,ReuseAddr=>1) or die $!;
while (my $c=$s->accept) { <$c>; print $c "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"; close $c; }
"##;
        let dir = fixture(server);
        let root = dir.path().canonicalize().unwrap();
        let driver = SenseVoiceGgufDriver::new("sensevoice-small", root);
        driver.start(0).await.unwrap();
        assert!(driver.health().await);
        assert_eq!(
            driver.start(0).await.unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        driver.stop().await.unwrap();
        assert!(!driver.health().await);
        driver.stop().await.unwrap();
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn every_restart_reverifies_the_gguf_artifact() {
        let server = br##"#!/usr/bin/env perl
use IO::Socket::INET;
my $s = IO::Socket::INET->new(LocalAddr=>'127.0.0.1',LocalPort=>$ARGV[-1],Proto=>'tcp',Listen=>8,ReuseAddr=>1) or die $!;
while (my $c=$s->accept) { <$c>; print $c "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"; close $c; }
"##;
        let dir = fixture(server);
        let root = dir.path().canonicalize().unwrap();
        let catalog_id = "sensevoice-small-gguf-q8";
        let artifact = verify_legacy_gguf_artifact(&root, catalog_id).unwrap();
        let requirements = ArtifactRequirements {
            identity: artifact.identity.clone(),
            manifest_sha256: artifact.manifest_sha256.clone(),
            roles: artifact
                .files
                .iter()
                .map(|(role, file)| (role.clone(), file.executable))
                .collect(),
            architectures: BTreeMap::new(),
        };
        assert_ne!(artifact.identity.catalog_id, artifact.identity.family);
        let driver =
            SenseVoiceGgufDriver::new_verified(catalog_id, root, requirements, artifact.size_bytes);
        driver.start(0).await.unwrap();
        driver.stop().await.unwrap();
        // Same-size replacement proves restart performs a fresh content hash,
        // rather than trusting an installed-state metadata/cache decision.
        fs::write(dir.path().join("sensevoice.gguf"), b"other").unwrap();
        assert_eq!(
            driver.start(0).await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(!driver.health().await);
    }
    #[tokio::test]
    #[cfg(unix)]
    async fn health_timeout_reaps_exited_child() {
        let dir = fixture(b"#!/bin/sh\nexit 0\n");
        let root = dir.path().canonicalize().unwrap();
        let driver = SenseVoiceGgufDriver {
            id: "sensevoice-small".into(),
            root,
            requirements: None,
            expected_size_bytes: None,
            health_timeout: Duration::from_millis(30),
            request_timeout: Duration::from_secs(1),
            proc: Mutex::new(None),
        };
        assert_eq!(
            driver.start(0).await.unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(!driver.health().await);
        driver.stop().await.unwrap();
    }
    #[tokio::test]
    #[cfg(unix)]
    async fn health_response_timeout_reaps_child() {
        let hanging = br##"#!/usr/bin/env perl
use IO::Socket::INET;
my $s = IO::Socket::INET->new(LocalAddr=>'127.0.0.1',LocalPort=>$ARGV[-1],Proto=>'tcp',Listen=>1,ReuseAddr=>1) or die $!;
my $c=$s->accept; sleep 60;
"##;
        let dir = fixture(hanging);
        let root = dir.path().canonicalize().unwrap();
        let driver = SenseVoiceGgufDriver {
            id: "sensevoice-small".into(),
            root,
            requirements: None,
            expected_size_bytes: None,
            health_timeout: Duration::from_millis(80),
            request_timeout: Duration::from_secs(1),
            proc: Mutex::new(None),
        };
        let began = tokio::time::Instant::now();
        assert_eq!(
            driver.start(0).await.unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(
            began.elapsed() < Duration::from_secs(1),
            "health deadline must bound a non-responsive peer"
        );
        assert!(!driver.health().await);
        driver.stop().await.unwrap();
    }
    #[tokio::test]
    async fn multipart_verbose_json_is_normalized_without_words() {
        use axum::{extract::Multipart, routing::post, Json, Router};
        use std::sync::{Arc, Mutex as StdMutex};
        let fields = Arc::new(StdMutex::new(Vec::new()));
        let file = Arc::new(StdMutex::new(None));
        let observed = fields.clone();
        let observed_file = file.clone();
        let app = Router::new().route("/v1/audio/transcriptions", post(move |mut multipart: Multipart| { let observed = observed.clone(); async move {
            let mut received = Vec::new(); while let Ok(Some(field)) = multipart.next_field().await { let name = field.name().unwrap_or("").to_string(); if name == "file" { let metadata = (field.file_name().map(str::to_owned), field.content_type().map(str::to_owned), field.bytes().await.unwrap_or_default().to_vec()); *observed_file.lock().unwrap() = Some(metadata); received.push((name, String::new())); } else { received.push((name, field.text().await.unwrap_or_default())); } } observed.lock().unwrap().extend(received);
            Json(serde_json::json!({"text":"你好","segments":[{"id":0,"start":0.2,"end":0.8,"text":"你好"}],"words":[{"start":0.2,"end":0.3,"text":"你"}]}))
        }}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let wav = tempfile::NamedTempFile::new().unwrap();
        fs::write(wav.path(), b"wav-bytes").unwrap();
        let result = post_transcription(
            &format!("http://127.0.0.1:{port}/v1/audio/transcriptions"),
            &TranscribeReq {
                wav: wav.path().into(),
                language: Some("zh".into()),
                prompt: None,
                punc: Some(true),
                spk: Some(true),
            },
            "sensevoice-small",
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(result.segments.len(), 1);
        assert!(result.words.is_empty());
        let mut fields = fields.lock().unwrap().clone();
        fields.sort();
        assert_eq!(
            fields,
            vec![
                ("file".into(), "".into()),
                ("model".into(), "sensevoice-small".into()),
                ("response_format".into(), "verbose_json".into())
            ]
        );
        assert_eq!(
            *file.lock().unwrap(),
            Some((
                Some("audio.wav".into()),
                Some("audio/wav".into()),
                b"wav-bytes".to_vec()
            ))
        );
        task.abort();
    }
    #[tokio::test]
    async fn bad_json_and_http_status_are_retryable() {
        use axum::{http::StatusCode, routing::post, Router};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = Router::new()
            .route("/bad", post(|| async { "not json" }))
            .route("/empty", post(|| async { "{\"text\":\"x\"}" }))
            .route(
                "/no-speech",
                post(|| async { "{\"text\":\"\",\"segments\":[],\"words\":[]}" }),
            )
            .route("/status", post(|| async { StatusCode::BAD_GATEWAY }))
            .route(
                "/slow",
                post(|| async {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    "late"
                }),
            );
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let wav = tempfile::NamedTempFile::new().unwrap();
        fs::write(wav.path(), b"wav").unwrap();
        let req = TranscribeReq {
            wav: wav.path().into(),
            language: None,
            prompt: None,
            punc: None,
            spk: None,
        };
        let bad = post_transcription(
            &format!("http://127.0.0.1:{port}/bad"),
            &req,
            "sensevoice-small",
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(matches!(bad, RuntimeError::Decode(_)) && bad.is_retryable());
        let status = post_transcription(
            &format!("http://127.0.0.1:{port}/status"),
            &req,
            "sensevoice-small",
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(matches!(status, RuntimeError::BadStatus(_)) && status.is_retryable());
        let empty = post_transcription(
            &format!("http://127.0.0.1:{port}/empty"),
            &req,
            "sensevoice-small",
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(matches!(empty, RuntimeError::Protocol(_)) && empty.is_retryable());
        let no_speech = post_transcription(
            &format!("http://127.0.0.1:{port}/no-speech"),
            &req,
            "sensevoice-small",
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert!(no_speech.text.is_empty());
        assert!(no_speech.segments.is_empty());
        assert!(no_speech.words.is_empty());
        let began = tokio::time::Instant::now();
        let timeout = post_transcription(
            &format!("http://127.0.0.1:{port}/slow"),
            &req,
            "sensevoice-small",
            Duration::from_millis(30),
        )
        .await
        .unwrap_err();
        match &timeout {
            RuntimeError::Http(error) => assert!(error.is_timeout()),
            other => panic!("expected HTTP timeout, got {other:?}"),
        };
        assert!(
            began.elapsed() >= Duration::from_millis(20)
                && began.elapsed() < Duration::from_secs(1)
        );
        assert!(timeout.is_retryable());
        task.abort();
    }

    #[test]
    fn response_contract_rejects_one_sided_and_invalid_segment_results() {
        use crate::contract::OpenAiSegment;

        let mut text_only = OpenAiSegments {
            text: "hello".into(),
            segments: vec![],
            words: vec![],
        };
        assert!(matches!(
            validate_response(&mut text_only),
            Err(RuntimeError::Protocol(_))
        ));

        let mut segment_only = OpenAiSegments {
            text: String::new(),
            segments: vec![OpenAiSegment {
                start: 0.0,
                end: 0.5,
                text: "hello".into(),
                speaker: None,
            }],
            words: vec![],
        };
        assert!(matches!(
            validate_response(&mut segment_only),
            Err(RuntimeError::Protocol(_))
        ));

        let mut reversed = OpenAiSegments {
            text: "hello".into(),
            segments: vec![OpenAiSegment {
                start: 0.5,
                end: 0.1,
                text: "hello".into(),
                speaker: None,
            }],
            words: vec![],
        };
        assert!(matches!(
            validate_response(&mut reversed),
            Err(RuntimeError::Protocol(_))
        ));
    }
    #[tokio::test]
    #[cfg(unix)]
    async fn protocol_failure_stops_the_sidecar() {
        let server = br##"#!/usr/bin/env perl
use IO::Socket::INET;
my $s = IO::Socket::INET->new(LocalAddr=>'127.0.0.1',LocalPort=>$ARGV[-1],Proto=>'tcp',Listen=>8,ReuseAddr=>1) or die $!;
while (my $c=$s->accept) { <$c>; print $c "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"; close $c; }
"##;
        let dir = fixture(server);
        let driver =
            SenseVoiceGgufDriver::new("sensevoice-small", dir.path().canonicalize().unwrap());
        driver.start(0).await.unwrap();
        let wav = tempfile::NamedTempFile::new().unwrap();
        fs::write(wav.path(), b"wav").unwrap();
        let request = TranscribeReq {
            wav: wav.path().into(),
            language: None,
            prompt: None,
            punc: None,
            spk: None,
        };
        assert!(driver.transcribe(request).await.is_err());
        assert!(!driver.health().await);
    }
}
