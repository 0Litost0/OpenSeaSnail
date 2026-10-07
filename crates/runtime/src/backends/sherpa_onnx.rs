//! SenseVoiceSmall sherpa-onnx sidecar lifecycle and protocol driver.

use super::sherpa_protocol::{
    parse_error_response, parse_health_response, parse_transcribe_response, visible_words,
    AUDIO_CONTENT_TYPE, CAPABILITY_HEADER, HEALTH_DEADLINE_SECS, HEALTH_PATH, MAX_AUDIO_BYTES,
    MAX_RESPONSE_BYTES, PROTOCOL_HEADER, PROTOCOL_VERSION, TRANSCRIBE_DEADLINE_SECS,
    TRANSCRIBE_PATH,
};
use crate::contract::{OpenAiSegment, OpenAiSegments, TranscribeReq};
use crate::{
    verify_artifact, ArtifactRequirements, Capabilities, Diarization, ModelRuntime, RuntimeError,
    RuntimeKind, SidecarProcess,
};
use async_trait::async_trait;
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::TcpListener;
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

pub struct SherpaOnnxDriver {
    id: String,
    root: PathBuf,
    requirements: ArtifactRequirements,
    state: Mutex<Option<Handle>>,
    health_timeout: Duration,
    request_timeout: Duration,
}

struct Handle {
    process: SidecarProcess,
    /// Retaining the exact socket makes listener identity independent of a
    /// racy free-port handoff or a process-table inspection utility.
    _listener: TcpListener,
    port: u16,
    capability: String,
}

impl SherpaOnnxDriver {
    pub(crate) fn new(id: &str, root: PathBuf, requirements: ArtifactRequirements) -> Self {
        Self {
            id: id.into(),
            root,
            requirements,
            state: Mutex::new(None),
            health_timeout: Duration::from_secs(HEALTH_DEADLINE_SECS),
            request_timeout: Duration::from_secs(TRANSCRIBE_DEADLINE_SECS),
        }
    }
}

#[async_trait]
impl ModelRuntime for SherpaOnnxDriver {
    fn id(&self) -> &str {
        &self.id
    }

    fn runtime_kind(&self) -> RuntimeKind {
        RuntimeKind::SherpaOnnx
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            diarization: Diarization::None,
            streaming: false,
            languages: vec!["zh".into(), "en".into(), "mixed".into()],
            max_audio_seconds: 3600,
            word_timestamps: true,
        }
    }

    async fn start(&self, requested_port: u16) -> io::Result<()> {
        let mut state = self.state.lock().await;
        if state.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Sherpa runtime already started; stop() first",
            ));
        }
        let root = self.root.clone();
        let requirements = self.requirements.clone();
        let artifact = tokio::task::spawn_blocking(move || verify_artifact(&root, &requirements))
            .await
            .map_err(|error| {
                io::Error::other(format!("Sherpa artifact verification task failed: {error}"))
            })?
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        let role = |name: &str| {
            artifact.file(name).map(PathBuf::from).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Sherpa artifact role {name:?} missing"),
                )
            })
        };
        let sidecar = role("sidecar")?;
        let model = role("model")?;
        let tokens = role("tokens")?;
        let vad = role("vad")?;
        let variant = artifact.identity.variant.clone();
        let manifest_hash_prefix = artifact.manifest_sha256[..12].to_owned();

        let listener = TcpListener::bind(("127.0.0.1", requested_port))?;
        let port = listener.local_addr()?.port();
        let capability = generate_capability()?;
        let (mut capability_writer, capability_reader) = UnixStream::pair()?;
        let listener_fd = listener.as_raw_fd();
        let capability_fd = capability_reader.as_raw_fd();
        let mut command = tokio::process::Command::new(&sidecar);
        command.args([
            "--listener-fd",
            &listener_fd.to_string(),
            "--capability-fd",
            &capability_fd.to_string(),
            "--catalog-id",
            &self.id,
            "--model",
            &model.to_string_lossy(),
            "--tokens",
            &tokens.to_string_lossy(),
            "--vad-model",
            &vad.to_string_lossy(),
            "--num-threads",
            "1",
        ]);
        // Only these two explicitly selected descriptors cross exec. The
        // capability writer and every unrelated descriptor retain CLOEXEC.
        unsafe {
            command.as_std_mut().pre_exec(move || {
                clear_cloexec(listener_fd)?;
                clear_cloexec(capability_fd)?;
                Ok(())
            });
        }
        let mut process = SidecarProcess::spawn(command, "sherpa-onnx").await?;
        drop(capability_reader);
        if let Err(error) = capability_writer.write_all(capability.as_bytes()) {
            let _ = process.stop().await;
            return Err(error);
        }
        drop(capability_writer);
        if !wait_health(port, &capability, &self.id, self.health_timeout).await {
            let _ = process.stop().await;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Sherpa sidecar health timeout",
            ));
        }
        tracing::info!(backend = "sherpa_onnx", catalog_id = %self.id, %variant,
            %manifest_hash_prefix, "sidecar ready");
        *state = Some(Handle {
            process,
            _listener: listener,
            port,
            capability,
        });
        Ok(())
    }

    async fn stop(&self) -> io::Result<()> {
        if let Some(mut handle) = self.state.lock().await.take() {
            let result = handle.process.stop().await;
            let code = result?;
            tracing::info!(backend = "sherpa_onnx", catalog_id = %self.id, ?code, "sidecar stopped");
        }
        Ok(())
    }

    async fn health(&self) -> bool {
        let state = self.state.lock().await;
        match state.as_ref() {
            Some(handle) => {
                health_once(
                    handle.port,
                    &handle.capability,
                    &self.id,
                    Duration::from_millis(500),
                )
                .await
            }
            None => false,
        }
    }

    async fn transcribe(&self, req: TranscribeReq) -> Result<OpenAiSegments, RuntimeError> {
        let state = self.state.lock().await;
        let handle = state.as_ref().ok_or(RuntimeError::NotStarted)?;
        let began = Instant::now();
        let result = post_transcription(
            handle.port,
            &handle.capability,
            &self.id,
            &req,
            self.request_timeout,
        )
        .await;
        match &result {
            Ok(value) => tracing::info!(backend = "sherpa_onnx", catalog_id = %self.id,
                segments = value.segments.len(), elapsed_ms = began.elapsed().as_millis() as u64,
                "transcription completed"),
            Err(error) => tracing::warn!(backend = "sherpa_onnx", catalog_id = %self.id,
                class = error_class(error), reason = error_reason(error),
                elapsed_ms = began.elapsed().as_millis() as u64,
                "transcription failed"),
        }
        result
    }
}

fn generate_capability() -> io::Result<String> {
    let mut bytes = [0_u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(unix)]
fn clear_cloexec(fd: libc::c_int) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

async fn wait_health(port: u16, capability: &str, catalog_id: &str, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if health_once(
            port,
            capability,
            catalog_id,
            remaining.min(Duration::from_secs(1)),
        )
        .await
        {
            return true;
        }
        tokio::time::sleep(
            Duration::from_millis(50)
                .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
        )
        .await;
    }
    false
}

async fn health_once(port: u16, capability: &str, catalog_id: &str, timeout: Duration) -> bool {
    let client = match reqwest::Client::builder().timeout(timeout).build() {
        Ok(client) => client,
        Err(_) => return false,
    };
    let response = match client
        .get(format!("http://127.0.0.1:{port}{HEALTH_PATH}"))
        .header(CAPABILITY_HEADER, capability)
        .header(PROTOCOL_HEADER, PROTOCOL_VERSION)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => response,
        _ => return false,
    };
    if !response_is_json(&response) {
        return false;
    }
    match read_bounded_response(response).await {
        Ok(body) => parse_health_response(&body, catalog_id).is_ok(),
        Err(_) => false,
    }
}

async fn post_transcription(
    port: u16,
    capability: &str,
    catalog_id: &str,
    req: &TranscribeReq,
    timeout: Duration,
) -> Result<OpenAiSegments, RuntimeError> {
    let metadata = tokio::fs::metadata(&req.wav).await?;
    if metadata.len() > MAX_AUDIO_BYTES as u64 {
        return Err(RuntimeError::Protocol(
            "WAV request exceeds the locked size limit".into(),
        ));
    }
    let wav = tokio::fs::read(&req.wav).await?;
    if wav.len() > MAX_AUDIO_BYTES {
        return Err(RuntimeError::Protocol(
            "WAV request exceeds the locked size limit".into(),
        ));
    }
    #[cfg(all(debug_assertions, feature = "sherpa-fixture-diagnostics"))]
    let diagnostic_fixture = locked_diagnostic_fixture(&wav);
    let client = reqwest::Client::builder().timeout(timeout).build()?;
    let response = client
        .post(format!("http://127.0.0.1:{port}{TRANSCRIBE_PATH}"))
        .header(CAPABILITY_HEADER, capability)
        .header(PROTOCOL_HEADER, PROTOCOL_VERSION)
        .header(reqwest::header::CONTENT_TYPE, AUDIO_CONTENT_TYPE)
        .body(wav)
        .send()
        .await?;
    let status = response.status();
    if !response_is_json(&response) {
        return Err(RuntimeError::Protocol(
            "sidecar response Content-Type is not application/json".into(),
        ));
    }
    let body = read_bounded_response(response).await?;
    if !status.is_success() {
        return match parse_error_response(&body) {
            Ok(error) if error.error.code.http_status() == status.as_u16() => {
                Err(RuntimeError::Protocol(format!("{:?}", error.error.code)))
            }
            Ok(_) => Err(RuntimeError::Protocol(
                "sidecar error status and code disagree".into(),
            )),
            Err(_) => Err(RuntimeError::BadStatus(status)),
        };
    }
    let response = parse_transcribe_response(&body, catalog_id)
        .map_err(|error| RuntimeError::Protocol(error.to_string()))?;
    let decoded_tokens: usize = response
        .segments
        .iter()
        .map(|segment| segment.decoded_tokens.len())
        .sum();
    let token_timestamps: usize = response
        .segments
        .iter()
        .map(|segment| segment.token_start_seconds.as_ref().map_or(0, Vec::len))
        .sum();
    tracing::debug!(
        backend = "sherpa_onnx",
        catalog_id,
        segments = response.segments.len(),
        decoded_tokens,
        token_timestamps,
        "sidecar response validated"
    );
    let segments_match_text = response
        .segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<String>()
        == response.text;
    let words = if segments_match_text {
        match visible_words(&response) {
            Ok(words) => words,
            Err(reason) => {
                tracing::debug!(
                    backend = "sherpa_onnx",
                    catalog_id,
                    fallback_reason = reason.as_str(),
                    "Sherpa token timeline is not used for word anchoring"
                );
                Vec::new()
            }
        }
    } else {
        tracing::debug!(
            backend = "sherpa_onnx",
            catalog_id,
            fallback_reason = "segment_text_mismatch",
            "sidecar segments do not exactly reconstruct authoritative text"
        );
        Vec::new()
    };
    #[cfg(all(debug_assertions, feature = "sherpa-fixture-diagnostics"))]
    if diagnostic_fixture {
        tracing::debug!(fixture = "locked-public-zh.wav", text = %response.text,
            segments = ?response.segments, "explicit fixed-fixture diagnostics");
    }
    let segments = response
        .segments
        .into_iter()
        .filter(|_| segments_match_text)
        .map(|segment| OpenAiSegment {
            start: segment.start_seconds,
            end: segment.end_seconds,
            text: segment.text,
            speaker: None,
        })
        .collect();
    // Token timelines remain private until M4 proves exact reconstruction.
    let mut result = OpenAiSegments {
        text: response.text,
        segments,
        words,
    };
    let had_words = !result.words.is_empty();
    result.validate_words();
    if had_words && result.words.is_empty() {
        tracing::debug!(
            backend = "sherpa_onnx",
            catalog_id,
            fallback_reason = "generic_words_validation_failed",
            "generic OpenAi word validation cleared Sherpa alignment"
        );
    }
    Ok(result)
}

fn response_is_json(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value.split(';').next().is_some_and(|media_type| {
                media_type.trim().eq_ignore_ascii_case("application/json")
            })
        })
}

async fn read_bounded_response(mut response: reqwest::Response) -> Result<Vec<u8>, RuntimeError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(RuntimeError::Protocol(
            "sidecar response exceeds the locked size limit".into(),
        ));
    }
    let mut body = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or(4096)
            .min(MAX_RESPONSE_BYTES as u64) as usize,
    );
    while let Some(chunk) = response.chunk().await? {
        if body
            .len()
            .checked_add(chunk.len())
            .is_none_or(|size| size > MAX_RESPONSE_BYTES)
        {
            return Err(RuntimeError::Protocol(
                "sidecar response exceeds the locked size limit".into(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(all(debug_assertions, feature = "sherpa-fixture-diagnostics"))]
fn locked_diagnostic_fixture(wav: &[u8]) -> bool {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(wav))
        == "b77f1794fe374a0ba1ee1dc458bfaf9349496cbbfc32780c50ba3c5a7ad8e373"
}

fn error_class(error: &RuntimeError) -> &'static str {
    match error {
        RuntimeError::Io(_) => "io",
        RuntimeError::Http(_) => "http",
        RuntimeError::NotStarted => "not_started",
        RuntimeError::HealthTimeout => "health_timeout",
        RuntimeError::BadStatus(_) => "bad_status",
        RuntimeError::Decode(_) => "decode",
        RuntimeError::Protocol(_) => "protocol",
        RuntimeError::SidecarLost(_) => "sidecar_lost",
    }
}

fn error_reason(error: &RuntimeError) -> &'static str {
    match error {
        RuntimeError::Protocol(message) if message == "InvalidAudio" => "invalid_audio",
        RuntimeError::Protocol(message)
            if message.contains("empty text or invalid segment count") =>
        {
            "empty_transcript"
        }
        RuntimeError::Protocol(_) => "protocol_invalid",
        _ => error_class(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ArtifactIdentity;
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn hash(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    #[test]
    fn protocol_error_reason_is_safe_and_actionable() {
        assert_eq!(
            error_reason(&RuntimeError::Protocol("InvalidAudio".into())),
            "invalid_audio"
        );
        assert_eq!(
            error_reason(&RuntimeError::Protocol(
                "empty text or invalid segment count".into()
            )),
            "empty_transcript"
        );
        assert_eq!(
            error_reason(&RuntimeError::Protocol("unexpected private detail".into())),
            "protocol_invalid"
        );
    }

    fn fixture_with_sidecar(
        override_sidecar: Option<&[u8]>,
    ) -> (tempfile::TempDir, ArtifactRequirements) {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = br##"#!/usr/bin/env python3
import json, os, socket, sys
a=sys.argv
def value(name): return a[a.index(name)+1]
listener=socket.fromfd(int(value('--listener-fd')), socket.AF_INET, socket.SOCK_STREAM)
cap=os.read(int(value('--capability-fd')),64).decode()
catalog=value('--catalog-id')
while True:
 c,_=listener.accept(); data=b''
 while b'\r\n\r\n' not in data: data+=c.recv(4096)
 head,body=data.split(b'\r\n\r\n',1); lines=head.decode().split('\r\n'); headers={}
 for line in lines[1:]:
  k,v=line.split(':',1); headers[k.lower()]=v.strip()
 length=int(headers.get('content-length','0'))
 while len(body)<length: body+=c.recv(length-len(body))
 if headers.get('x-seasnail-capability') != cap: status='401 Unauthorized'; payload={'protocol_version':1,'error':{'code':'unauthorized','retryable':False,'message':'unauthorized'}}
 elif lines[0].startswith('GET /health '): status='200 OK'; payload={'protocol_version':1,'status':'ready','catalog_id':catalog}
 else: status='200 OK'; payload={'protocol_version':1,'catalog_id':catalog,'text':'hello','segments':[{'start_seconds':0.1,'end_seconds':0.8,'text':'hello','decoded_tokens':['hello'],'token_start_seconds':[0.1],'language':'en','event':None}],'transforms':{'use_itn':True,'rule_fsts':False,'homophone_replacer':False,'post_decode_text_modified':False}}
 encoded=json.dumps(payload,separators=(',',':')).encode(); c.sendall(('HTTP/1.1 '+status+'\r\nContent-Type: application/json\r\nContent-Length: '+str(len(encoded))+'\r\nConnection: close\r\n\r\n').encode()+encoded); c.close()
"##;
        let sidecar = override_sidecar.unwrap_or(sidecar);
        let entries: [(&str, &str, &[u8], bool); 6] = [
            ("sidecar", "sidecar.py", sidecar, true),
            ("model", "model.onnx", b"model", false),
            ("tokens", "tokens.txt", b"tokens", false),
            ("vad", "vad.onnx", b"vad", false),
            ("onnxruntime", "libonnxruntime.dylib", b"ort", true),
            ("sherpa-onnx", "libsherpa.dylib", b"sherpa", true),
        ];
        let mut files = Vec::new();
        for (role, path, bytes, executable) in entries {
            fs::write(dir.path().join(path), bytes).unwrap();
            if executable {
                fs::set_permissions(dir.path().join(path), fs::Permissions::from_mode(0o755))
                    .unwrap();
            }
            files.push(serde_json::json!({"role":role,"path":path,"sha256":hash(bytes),"size_bytes":bytes.len(),"executable":executable}));
        }
        let manifest = serde_json::json!({
            "schema_version":1,"runtime":"sherpa_onnx","catalog_id":"sensevoice-small-sherpa-int8",
            "family":"sensevoice-small","variant":"int8","api_contract_version":1,
            "source_revisions":{"sherpa-onnx":"0123456789abcdef0123456789abcdef01234567"},"files":files
        }).to_string();
        fs::write(dir.path().join("artifact-manifest.json"), &manifest).unwrap();
        let requirements = ArtifactRequirements {
            identity: ArtifactIdentity {
                runtime: "sherpa_onnx".into(),
                catalog_id: "sensevoice-small-sherpa-int8".into(),
                family: "sensevoice-small".into(),
                variant: "int8".into(),
                api_contract_version: 1,
            },
            manifest_sha256: hash(manifest.as_bytes()),
            roles: BTreeMap::from(
                entries.map(|(role, _, _, executable)| (role.into(), executable)),
            ),
            architectures: BTreeMap::new(),
        };
        (dir, requirements)
    }

    fn fixture() -> (tempfile::TempDir, ArtifactRequirements) {
        fixture_with_sidecar(None)
    }

    #[tokio::test]
    async fn inherited_listener_capability_lifecycle_and_transcription() {
        let (dir, requirements) = fixture();
        let driver = SherpaOnnxDriver::new(
            "sensevoice-small-sherpa-int8",
            dir.path().canonicalize().unwrap(),
            requirements,
        );
        driver.start(0).await.unwrap();
        assert!(driver.health().await);
        assert_eq!(
            driver.start(0).await.unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        let wav = dir.path().join("sample.wav");
        fs::write(&wav, b"RIFF-test").unwrap();
        let result = driver
            .transcribe(TranscribeReq {
                wav,
                language: None,
                prompt: None,
                punc: None,
                spk: None,
            })
            .await
            .unwrap();
        assert_eq!(result.text, "hello");
        assert_eq!(result.segments.len(), 1);
        assert_eq!(result.words.len(), 1);
        assert_eq!(result.words[0].text, "hello");
        driver.stop().await.unwrap();
        assert!(!driver.health().await);
        driver.stop().await.unwrap();
    }

    #[tokio::test]
    async fn invalid_token_timestamps_degrade_to_alignment_fallback_not_no_speech() {
        use axum::{routing::post, Json, Router};
        let wav = tempfile::NamedTempFile::new().unwrap();
        fs::write(wav.path(), b"wav").unwrap();
        let (port, task) = test_server(Router::new().route(
            TRANSCRIBE_PATH,
            post(|| async {
                Json(serde_json::json!({
                    "protocol_version":1,"catalog_id":"sensevoice-small-sherpa-int8",
                    "text":"visible","segments":[{"start_seconds":0.1,
                    "end_seconds":0.8,"text":"visible","decoded_tokens":["visible"],
                    "token_start_seconds":[0.2,0.9],"language":"en","event":null}],
                    "transforms":{"use_itn":true,"rule_fsts":false,
                    "homophone_replacer":false,"post_decode_text_modified":false}
                }))
            }),
        ))
        .await;
        let result = post_transcription(
            port,
            &"a".repeat(64),
            "sensevoice-small-sherpa-int8",
            &request(wav.path().into()),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(result.text, "visible");
        assert_eq!(result.segments.len(), 1);
        assert!(
            result.words.is_empty(),
            "invalid token timeline must stay an alignment fallback"
        );
        task.abort();
    }

    #[tokio::test]
    async fn segment_text_mismatch_clears_segments_and_words_but_keeps_text() {
        use axum::{routing::post, Json, Router};
        let wav = tempfile::NamedTempFile::new().unwrap();
        fs::write(wav.path(), b"wav").unwrap();
        let (port, task) = test_server(Router::new().route(
            TRANSCRIBE_PATH,
            post(|| async {
                Json(serde_json::json!({
                    "protocol_version":1,"catalog_id":"sensevoice-small-sherpa-int8",
                    "text":"authoritative","segments":[{"start_seconds":0.1,
                    "end_seconds":0.8,"text":"different","decoded_tokens":["different"],
                    "token_start_seconds":[0.1],"language":"en","event":null}],
                    "transforms":{"use_itn":true,"rule_fsts":false,
                    "homophone_replacer":false,"post_decode_text_modified":false}
                }))
            }),
        ))
        .await;
        let result = post_transcription(
            port,
            &"a".repeat(64),
            "sensevoice-small-sherpa-int8",
            &request(wav.path().into()),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(result.text, "authoritative");
        assert!(result.segments.is_empty());
        assert!(result.words.is_empty());
        task.abort();
    }

    #[tokio::test]
    async fn every_restart_reverifies_the_artifact() {
        let (dir, requirements) = fixture();
        let driver = SherpaOnnxDriver::new(
            "sensevoice-small-sherpa-int8",
            dir.path().canonicalize().unwrap(),
            requirements,
        );
        driver.start(0).await.unwrap();
        driver.stop().await.unwrap();
        fs::write(dir.path().join("model.onnx"), b"tampered").unwrap();
        assert_eq!(
            driver.start(0).await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn child_early_exit_and_health_timeout_are_reaped() {
        let (dir, requirements) = fixture_with_sidecar(Some(b"#!/bin/sh\nexit 0\n"));
        let driver = SherpaOnnxDriver {
            id: "sensevoice-small-sherpa-int8".into(),
            root: dir.path().canonicalize().unwrap(),
            requirements,
            state: Mutex::new(None),
            health_timeout: Duration::from_millis(80),
            request_timeout: Duration::from_secs(1),
        };
        assert_eq!(
            driver.start(0).await.unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(!driver.health().await);
        driver.stop().await.unwrap();
    }

    async fn test_server(app: axum::Router) -> (u16, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (port, task)
    }

    fn request(path: PathBuf) -> TranscribeReq {
        TranscribeReq {
            wav: path,
            language: None,
            prompt: None,
            punc: None,
            spk: None,
        }
    }

    #[tokio::test]
    async fn non_success_and_timeout_are_retryable_while_empty_success_is_preserved() {
        use axum::{http::StatusCode, routing::post, Json, Router};
        let wav = tempfile::NamedTempFile::new().unwrap();
        fs::write(wav.path(), b"wav").unwrap();

        let (port, task) = test_server(Router::new().route(
            TRANSCRIBE_PATH,
            post(|| async {
                (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({"protocol_version":1,"error":{
                        "code":"busy","retryable":true,"message":"busy"
                    }})),
                )
            }),
        ))
        .await;
        let error = post_transcription(
            port,
            &"a".repeat(64),
            "sensevoice-small-sherpa-int8",
            &request(wav.path().into()),
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, RuntimeError::Protocol(_)) && error.is_retryable());
        task.abort();

        let (port, task) = test_server(Router::new().route(
            TRANSCRIBE_PATH,
            post(|| async {
                Json(serde_json::json!({
                    "protocol_version":1,"catalog_id":"sensevoice-small-sherpa-int8",
                    "text":"","segments":[],"transforms":{"use_itn":true,
                    "rule_fsts":false,"homophone_replacer":false,"post_decode_text_modified":false}
                }))
            }),
        ))
        .await;
        let empty = post_transcription(
            port,
            &"a".repeat(64),
            "sensevoice-small-sherpa-int8",
            &request(wav.path().into()),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert!(empty.text.is_empty());
        assert!(empty.segments.is_empty());
        assert!(empty.words.is_empty());
        task.abort();

        let (port, task) = test_server(Router::new().route(
            TRANSCRIBE_PATH,
            post(|| async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                "late"
            }),
        ))
        .await;
        let error = post_transcription(
            port,
            &"a".repeat(64),
            "sensevoice-small-sherpa-int8",
            &request(wav.path().into()),
            Duration::from_millis(30),
        )
        .await
        .unwrap_err();
        assert!(matches!(&error, RuntimeError::Http(inner) if inner.is_timeout()));
        assert!(error.is_retryable());
        task.abort();
    }
}
