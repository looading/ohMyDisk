//! Current-user local IPC. Every connection carries exactly one bounded request/response.

use crate::engine::Engine;
use fs2::FileExt;
use interprocess::local_socket::{
    ListenerOptions, Name,
    tokio::{Listener, Stream, prelude::*},
};
use ohmydisk_core::{AppError, NativePath, new_id};
use ohmydisk_platform::{
    current_user_identity, open_private_file, prepare_private_dir, secure_listener_options,
    spawn_service_process,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{Semaphore, watch},
    time::{Instant, timeout},
};

pub const PROTOCOL_VERSION: u32 = 1;
const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
const CONNECTION_LIMIT: usize = 16;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Serialize, Deserialize)]
struct Request {
    version: u32,
    id: String,
    operation: String,
    arguments: Value,
}
#[derive(Serialize, Deserialize)]
struct Response {
    version: u32,
    id: String,
    result: Value,
    error: Option<AppError>,
}

#[derive(Clone, Debug)]
pub struct Client {
    endpoint: Endpoint,
    data_dir: PathBuf,
    #[cfg(test)]
    test_launcher: Option<tokio::sync::mpsc::UnboundedSender<PathBuf>>,
}

impl Client {
    pub async fn connect_or_spawn(data_dir: PathBuf) -> Result<Self, AppError> {
        prepare_private_dir(&data_dir)?;
        let data_dir = fs::canonicalize(data_dir)?;
        let client = Self {
            endpoint: Endpoint::new(&data_dir)?,
            data_dir,
            #[cfg(test)]
            test_launcher: None,
        };
        client.ensure_service().await?;
        Ok(client)
    }

    async fn ensure_service(&self) -> Result<(), AppError> {
        match self.call_once("__ping", json!({})).await {
            Ok(_) => return Ok(()),
            Err(error)
                if error.code == "protocol_version_mismatch"
                    || error.code == "permission_denied" =>
            {
                return Err(error);
            }
            Err(_) => {}
        }
        // This lock coordinates spawn attempts. The service has a separate lifetime lock.
        let startup_lock = open_private_file(&self.data_dir.join("startup.lock"))?;
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut spawned = false;
        let mut spawn_error = None;
        loop {
            match self.call_once("__ping", json!({})).await {
                Ok(_) => return Ok(()),
                Err(error)
                    if error.code == "protocol_version_mismatch"
                        || error.code == "permission_denied" =>
                {
                    return Err(error);
                }
                Err(_) => {}
            }
            if !spawned && FileExt::try_lock_exclusive(&startup_lock).is_ok() {
                // Another client may have started the service immediately before this lock.
                if self.call_once("__ping", json!({})).await.is_ok() {
                    return Ok(());
                }
                if let Err(error) = self.spawn_service() {
                    spawn_error = Some(error.to_string());
                }
                spawned = true;
            }
            if Instant::now() >= deadline {
                return Err(AppError::new(
                    "service_unavailable",
                    spawn_error.unwrap_or_else(|| {
                        format!(
                            "Background service did not become ready; inspect {}",
                            self.data_dir.join("service.log").display()
                        )
                    }),
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn spawn_service(&self) -> io::Result<()> {
        #[cfg(test)]
        if let Some(launcher) = &self.test_launcher {
            return launcher
                .send(self.data_dir.clone())
                .map_err(io::Error::other);
        }
        let mut child = spawn_service_process(&self.data_dir)?;
        // A Tokio blocking task would delay runtime shutdown until the service exits.
        // An ordinary detached reaper thread does not retain the client.
        let _ = std::thread::Builder::new()
            .name("ohmydisk-service-reaper".into())
            .spawn(move || {
                let _ = child.wait();
            });
        Ok(())
    }

    pub async fn call(&self, operation: &str, arguments: Value) -> Result<Value, AppError> {
        timeout(REQUEST_TIMEOUT, async {
            // The service may have exited while this long-lived GUI/MCP client was idle.
            // Retry only establishment: no application request has been sent at this point.
            let stream = match self.connect().await {
                Ok(stream) => stream,
                Err(error) if error.code == "service_unavailable" => {
                    self.ensure_service().await?;
                    self.connect().await?
                },
                Err(error) => return Err(error),
            };
            self.exchange(stream, operation, arguments).await
        }).await.map_err(|_| AppError::new("service_unavailable", "Service request timed out; use the same request_key when retrying a state-changing operation"))?
    }

    async fn connect(&self) -> Result<Stream, AppError> {
        let stream = timeout(CONNECT_TIMEOUT, Stream::connect(self.endpoint.name()?))
            .await
            .map_err(|_| AppError::new("service_unavailable", "Service connection timed out"))?
            .map_err(connection_error)?;
        authenticate_peer(&stream)?;
        Ok(stream)
    }

    // Startup probes must not call public `call`, which would recurse into ensure_service.
    async fn call_once(&self, operation: &str, arguments: Value) -> Result<Value, AppError> {
        let stream = self.connect().await?;
        timeout(REQUEST_TIMEOUT, self.exchange(stream, operation, arguments))
            .await
            .map_err(|_| AppError::new("service_unavailable", "Service probe timed out"))?
    }

    async fn exchange(
        &self,
        mut stream: Stream,
        operation: &str,
        arguments: Value,
    ) -> Result<Value, AppError> {
        let request = Request {
            version: PROTOCOL_VERSION,
            id: new_id(),
            operation: operation.into(),
            arguments,
        };
        write_frame(&mut stream, &request).await?;
        // From the first write onward, an error may mean a request was accepted.
        // Never replay it implicitly; callers retain their original request_key for recovery.
        let response: Response = read_frame(&mut stream).await?;
        if response.version != PROTOCOL_VERSION {
            return Err(version_error(response.version));
        }
        if response.id != request.id {
            return Err(AppError::new(
                "invalid_response",
                "Response request ID does not match",
            ));
        }
        match response.error {
            Some(error) => Err(error),
            None => Ok(response.result),
        }
    }
}

/// Hold this lock before opening/recovering SQLite, so a losing startup cannot alter live jobs.
pub fn acquire_service_lock(data_dir: &std::path::Path) -> Result<File, AppError> {
    prepare_private_dir(data_dir)?;
    let lock = open_private_file(&data_dir.join("service.lock"))?;
    FileExt::try_lock_exclusive(&lock).map_err(|error| {
        AppError::new(
            "service_unavailable",
            format!("Another service instance owns the state directory: {error}"),
        )
    })?;
    Ok(lock)
}

pub async fn run(data_dir: PathBuf, engine: Arc<Engine>) -> Result<(), AppError> {
    let lock = acquire_service_lock(&data_dir)?;
    run_with_lock(data_dir, engine, lock).await
}

/// The passed lock must have been obtained with `acquire_service_lock` before Engine::open.
pub async fn run_with_lock(
    data_dir: PathBuf,
    engine: Arc<Engine>,
    _service_lock: File,
) -> Result<(), AppError> {
    prepare_private_dir(&data_dir)?;
    let endpoint = Endpoint::new(&fs::canonicalize(data_dir)?)?;
    endpoint.clear_stale_socket().await?;
    let options = secure_listener_options(ListenerOptions::new().name(endpoint.name()?))?;
    let listener: Listener = options.create_tokio()?;
    let active = Arc::new(AtomicUsize::new(0));
    let permits = Arc::new(Semaphore::new(CONNECTION_LIMIT));
    let (activity, idle_since) = watch::channel(Instant::now());
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            incoming = listener.accept() => {
                let stream = incoming?;
                if authenticate_peer(&stream).is_err() { continue; }
                let Ok(permit) = permits.clone().try_acquire_owned() else { continue };
                active.fetch_add(1, Ordering::SeqCst);
                activity.send_replace(Instant::now());
                let guard = ActiveConnection { active: active.clone(), activity: activity.clone() };
                let engine = engine.clone();
                tokio::spawn(async move {
                    let _guard = guard;
                    let _permit = permit;
                    if let Err(error) = serve_connection(stream, engine).await {
                        eprintln!("IPC connection ended: {}", error.code);
                    }
                });
            },
            _ = tick.tick() => {
                if engine.has_active_jobs() { activity.send_replace(Instant::now()); }
                if active.load(Ordering::SeqCst) == 0 && !engine.has_active_jobs() && idle_since.borrow().elapsed() >= IDLE_TIMEOUT { return Ok(()); }
            },
        }
    }
}

struct ActiveConnection {
    active: Arc<AtomicUsize>,
    activity: watch::Sender<Instant>,
}
impl Drop for ActiveConnection {
    fn drop(&mut self) {
        self.activity.send_replace(Instant::now());
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn serve_connection(mut stream: Stream, engine: Arc<Engine>) -> Result<(), AppError> {
    let request: Request = timeout(REQUEST_TIMEOUT, read_frame(&mut stream))
        .await
        .map_err(|_| AppError::new("request_timeout", "Incomplete request frame"))??;
    let result = if request.version != PROTOCOL_VERSION {
        Err(version_error(request.version))
    } else if request.id.len() > 64 || request.operation.len() > 128 {
        Err(AppError::new(
            "invalid_request",
            "Request ID or operation is too long",
        ))
    } else if request.operation == "__ping" {
        Ok(json!({"protocol_version":PROTOCOL_VERSION}))
    } else {
        engine.handle(&request.operation, request.arguments).await
    };
    let (result, error) = match result {
        Ok(value) => (value, None),
        Err(error) => (Value::Null, Some(error)),
    };
    let response = Response {
        version: PROTOCOL_VERSION,
        id: request.id,
        result,
        error,
    };
    match write_frame(&mut stream, &response).await {
        Err(error) if error.code == "frame_too_large" => {
            write_frame(
                &mut stream,
                &Response {
                    version: PROTOCOL_VERSION,
                    id: response.id,
                    result: Value::Null,
                    error: Some(error),
                },
            )
            .await
        }
        other => other,
    }
}

async fn read_frame<R: AsyncRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
) -> Result<T, AppError> {
    let length = reader.read_u32_le().await? as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(AppError::new(
            "frame_too_large",
            "IPC frame must contain between 1 byte and 4 MiB",
        ));
    }
    let mut bytes = vec![0u8; length];
    reader.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes)
        .map_err(|error| AppError::new("invalid_request", format!("Invalid IPC JSON: {error}")))
}

async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> Result<(), AppError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| AppError::new("invalid_request", error.to_string()))?;
    if bytes.is_empty() || bytes.len() > MAX_FRAME_BYTES {
        return Err(AppError::new(
            "frame_too_large",
            "IPC frame exceeds 4 MiB; request a smaller page",
        ));
    }
    writer.write_u32_le(bytes.len() as u32).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

fn authenticate_peer(stream: &Stream) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        let uid: u32 = current_user_identity()?
            .parse()
            .map_err(|_| AppError::new("permission_denied", "Cannot determine current UID"))?;
        if stream.peer_creds()?.euid() != Some(uid) {
            return Err(AppError::new(
                "permission_denied",
                "IPC peer belongs to another user",
            ));
        }
    }
    #[cfg(windows)]
    {
        let pid = stream.peer_creds()?.pid().ok_or_else(|| {
            AppError::new(
                "permission_denied",
                "IPC peer process identity is unavailable",
            )
        })?;
        ohmydisk_platform::verify_process_user(pid)
            .map_err(|error| AppError::new("permission_denied", error.to_string()))?;
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct Endpoint {
    name: String,
}
impl Endpoint {
    fn new(data_dir: &std::path::Path) -> Result<Self, AppError> {
        let digest = Sha256::digest(NativePath::new(data_dir).encoded.as_bytes());
        let short = digest[..12]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let identity = current_user_identity()?;
        #[cfg(unix)]
        {
            // Darwin's sockaddr_un path is short; use a validated private directory under /tmp.
            let directory = PathBuf::from("/tmp").join(format!("ohmydisk-{identity}-{short}"));
            prepare_private_dir(&directory)?;
            Ok(Self {
                name: directory
                    .join("s")
                    .to_str()
                    .ok_or_else(|| AppError::new("invalid_path", "Invalid socket path"))?
                    .into(),
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                name: format!("ohmydisk-v{PROTOCOL_VERSION}-{identity}-{short}"),
            })
        }
    }

    fn name(&self) -> io::Result<Name<'static>> {
        #[cfg(unix)]
        {
            use interprocess::local_socket::{GenericFilePath, ToFsName};
            self.name
                .clone()
                .to_fs_name::<GenericFilePath>()
                .map(Name::into_owned)
        }
        #[cfg(windows)]
        {
            use interprocess::local_socket::{GenericNamespaced, ToNsName};
            self.name
                .clone()
                .to_ns_name::<GenericNamespaced>()
                .map(Name::into_owned)
        }
    }

    async fn clear_stale_socket(&self) -> Result<(), AppError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            let metadata = match fs::symlink_metadata(&self.name) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            if !metadata.file_type().is_socket() {
                return Err(AppError::new(
                    "permission_denied",
                    "Existing IPC endpoint is not a socket",
                ));
            }
            match timeout(CONNECT_TIMEOUT, Stream::connect(self.name()?)).await {
                Ok(Err(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                    ) =>
                {
                    fs::remove_file(&self.name)?
                }
                _ => {
                    return Err(AppError::new(
                        "service_unavailable",
                        "An active or unresponsive endpoint already exists; refusing to unlink it",
                    ));
                }
            }
        }
        Ok(())
    }
}

fn connection_error(error: io::Error) -> AppError {
    if error.kind() == io::ErrorKind::PermissionDenied {
        error.into()
    } else {
        AppError::new("service_unavailable", error.to_string())
    }
}
fn version_error(peer: u32) -> AppError {
    AppError::new(
        "protocol_version_mismatch",
        format!("Service protocol {peer} is incompatible with client protocol {PROTOCOL_VERSION}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frame_rejects_length_before_allocating_body() {
        let (mut writer, mut reader) = tokio::io::duplex(8);
        writer
            .write_u32_le((MAX_FRAME_BYTES + 1) as u32)
            .await
            .unwrap();
        let error = read_frame::<_, Value>(&mut reader).await.unwrap_err();
        assert_eq!(error.code, "frame_too_large");
    }

    #[tokio::test]
    async fn frame_round_trip_and_truncated_body() {
        let (mut writer, mut reader) = tokio::io::duplex(1024);
        let value = json!({"path":"quote\";$(literal)","bytes":123});
        write_frame(&mut writer, &value).await.unwrap();
        assert_eq!(read_frame::<_, Value>(&mut reader).await.unwrap(), value);
        writer.write_u32_le(100).await.unwrap();
        drop(writer);
        assert!(read_frame::<_, Value>(&mut reader).await.is_err());
    }

    #[test]
    fn lifetime_lock_has_single_owner() {
        let fixture = tempfile::tempdir().unwrap();
        let data = fixture.path().join("state");
        let first = acquire_service_lock(&data).unwrap();
        assert!(acquire_service_lock(&data).is_err());
        drop(first);
        assert!(acquire_service_lock(&data).is_ok());
    }

    #[tokio::test]
    async fn two_clients_share_server_and_disconnect_independently() {
        let fixture = tempfile::tempdir().unwrap();
        let data = fixture.path().join("state");
        let lock = acquire_service_lock(&data).unwrap();
        let engine =
            Engine::open(data.clone(), Arc::new(ohmydisk_platform::NativePlatform)).unwrap();
        let endpoint = Endpoint::new(&fs::canonicalize(&data).unwrap()).unwrap();
        let first = Client {
            endpoint: endpoint.clone(),
            data_dir: fs::canonicalize(&data).unwrap(),
            test_launcher: None,
        };
        let second = first.clone();
        let server = tokio::spawn(run_with_lock(data, engine, lock));
        let mut ready = false;
        for _ in 0..100 {
            if first.call_once("__ping", json!({})).await.is_ok() {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(ready);
        let mut wrong_version = Stream::connect(endpoint.name().unwrap()).await.unwrap();
        write_frame(
            &mut wrong_version,
            &Request {
                version: PROTOCOL_VERSION + 1,
                id: "version-test".into(),
                operation: "__ping".into(),
                arguments: json!({}),
            },
        )
        .await
        .unwrap();
        let mismatch: Response = read_frame(&mut wrong_version).await.unwrap();
        assert_eq!(mismatch.error.unwrap().code, "protocol_version_mismatch");
        #[cfg(unix)]
        assert!(endpoint.clear_stale_socket().await.is_err());
        let a = first.call("get_capabilities", json!({})).await.unwrap();
        drop(first);
        let b = second.call("get_capabilities", json!({})).await.unwrap();
        assert_eq!(a, b);
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn existing_client_restarts_a_stopped_service_before_sending_request() {
        let fixture = tempfile::tempdir().unwrap();
        let data_dir = fixture.path().join("state");
        prepare_private_dir(&data_dir).unwrap();
        let data_dir = fs::canonicalize(data_dir).unwrap();
        let (launcher, mut launch_requests) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();
        let (server_events, mut servers) = tokio::sync::mpsc::unbounded_channel();
        let supervisor = tokio::spawn(async move {
            while let Some(data) = launch_requests.recv().await {
                let lock = acquire_service_lock(&data).unwrap();
                let engine =
                    Engine::open(data.clone(), Arc::new(ohmydisk_platform::NativePlatform))
                        .unwrap();
                server_events
                    .send(tokio::spawn(run_with_lock(data, engine, lock)))
                    .unwrap();
            }
        });
        let client = Client {
            endpoint: Endpoint::new(&data_dir).unwrap(),
            data_dir,
            test_launcher: Some(launcher),
        };
        client.ensure_service().await.unwrap();
        let first_server = servers.recv().await.unwrap();
        let before = client.call("get_capabilities", json!({})).await.unwrap();
        first_server.abort();
        let _ = first_server.await;
        // Same Client, now with a missing endpoint. Only connection establishment is retried.
        let after = client.call("get_capabilities", json!({})).await.unwrap();
        assert_eq!(before, after);
        let second_server = servers.recv().await.unwrap();
        second_server.abort();
        let _ = second_server.await;
        supervisor.abort();
        let _ = supervisor.await;
    }

    #[tokio::test]
    async fn lost_response_never_replays_an_already_sent_operation() {
        let fixture = tempfile::tempdir().unwrap();
        let data_dir = fixture.path().join("state");
        prepare_private_dir(&data_dir).unwrap();
        let data_dir = fs::canonicalize(data_dir).unwrap();
        let endpoint = Endpoint::new(&data_dir).unwrap();
        let listener =
            secure_listener_options(ListenerOptions::new().name(endpoint.name().unwrap()))
                .unwrap()
                .create_tokio()
                .unwrap();
        let (launcher, mut launches) = tokio::sync::mpsc::unbounded_channel();
        let client = Client {
            endpoint,
            data_dir,
            test_launcher: Some(launcher),
        };
        let server = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            let request: Request = read_frame(&mut stream).await.unwrap();
            assert_eq!(request.operation, "execute_permanent");
            // Simulate a process failing after accepting the operation, before replying.
        });
        let result = client
            .call(
                "execute_permanent",
                json!({"request_key":"same-authorized-request"}),
            )
            .await;
        assert!(result.is_err());
        server.await.unwrap();
        assert!(
            launches.try_recv().is_err(),
            "No restart/replay after the first request write"
        );
    }
}
