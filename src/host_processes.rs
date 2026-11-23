use crate::process_details::{self, Failure, Reading};
use crate::services::{
    LinuxProcessSignalBackend, PinnedProcess, ProcessData, ProcessSignalBackend, ProcessTarget,
    Signal, SignalFailure,
};
use enzui_core::cancellation::CancelToken;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::cell::RefCell;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub(crate) const HELPER_ARGUMENT: &str = "--host-process-worker";
const REQUEST_LIMIT: usize = 1024;
const RESPONSE_LIMIT: usize = 16 * 1024 * 1024;
const CALL_TIMEOUT: Duration = Duration::from_secs(1);
const RETRY_DELAY: Duration = Duration::from_secs(5);

#[derive(Deserialize, Serialize)]
enum Request {
    Sample,
    Details(ProcessTarget),
    Pin(u32),
    Send(Signal),
    Exited,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct Sample {
    pub rows: Vec<ProcessData>,
    pub threads: u64,
}

#[derive(Deserialize, Serialize)]
struct PinIdentity {
    start: u64,
    uid: u32,
}

#[derive(Default)]
struct Worker {
    pin: Option<(Box<dyn PinnedProcess>, PinIdentity)>,
}

#[derive(Deserialize, Serialize)]
enum Response {
    Sample(Sample),
    Details(Reading),
    Pinned(Result<PinIdentity, SignalFailure>),
    Sent(Result<(), SignalFailure>),
    Exited(Result<bool, SignalFailure>),
}

pub(crate) fn in_flatpak() -> bool {
    *FLATPAK.get_or_init(|| Path::new("/.flatpak-info").is_file())
}

static FLATPAK: OnceLock<bool> = OnceLock::new();

pub(crate) fn is_direct_pid(pid: u32) -> bool {
    // The sandbox PID cannot identify the GUI in host process rows.
    !in_flatpak() && pid == std::process::id()
}

struct Paths {
    app: PathBuf,
    runtime: PathBuf,
}

impl Paths {
    fn parse(info: &str) -> io::Result<Self> {
        let mut in_instance = false;
        let mut app = None;
        let mut runtime = None;
        for line in info.lines() {
            if line.starts_with('[') {
                in_instance = line == "[Instance]";
            } else if in_instance {
                if let Some(path) = line.strip_prefix("app-path=") {
                    app = Some(path);
                } else if let Some(path) = line.strip_prefix("runtime-path=") {
                    runtime = Some(path);
                }
            }
        }
        fn absolute(value: Option<&str>) -> io::Result<PathBuf> {
            value
                .map(PathBuf::from)
                .filter(|path| {
                    path.is_absolute()
                        && path
                            .components()
                            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
                        && path.parent().is_some()
                })
                .ok_or_else(|| invalid("invalid Flatpak deployment path"))
        }
        Ok(Self {
            app: absolute(app)?,
            runtime: absolute(runtime)?,
        })
    }

    fn command(&self) -> io::Result<Command> {
        let (lib, loader) = match std::env::consts::ARCH {
            "x86_64" => ("x86_64-linux-gnu", "ld-linux-x86-64.so.2"),
            "aarch64" => ("aarch64-linux-gnu", "ld-linux-aarch64.so.1"),
            _ => return Err(invalid("unsupported Flatpak host architecture")),
        };
        let library_dir = self.runtime.join("lib").join(lib);
        let mut command = Command::new("/usr/bin/flatpak-spawn");
        command.args(["--host", "--watch-bus", "--clear-env", "--directory=/"]);
        for key in ["HOME", "USER", "LOGNAME", "LANG", "LC_ALL"] {
            if let Ok(value) = std::env::var(key) {
                command.arg(format!("--env={key}={value}"));
            }
        }
        // The host libc may be older than the Flatpak runtime libc.
        command
            .arg("--")
            .arg(library_dir.join(loader))
            .arg("--inhibit-cache")
            .arg("--library-path")
            .arg(library_dir)
            .arg(self.app.join("bin/task-manager-app"))
            .arg(HELPER_ARGUMENT);
        Ok(command)
    }
}

fn invalid(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

struct Client {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
}

impl Client {
    fn open() -> io::Result<Self> {
        let info = std::fs::read_to_string("/.flatpak-info")?;
        let mut child = Paths::parse(&info)?
            .command()?
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| invalid("missing host stdin"))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| invalid("missing host stdout"))?;
        Ok(Self {
            child,
            input,
            output,
        })
    }

    fn call(&mut self, request: Request, token: &CancelToken) -> io::Result<Response> {
        if token.is_terminal() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        write_packet(&mut self.input, &request, REQUEST_LIMIT)?;
        let deadline = Instant::now() + CALL_TIMEOUT;
        read_response(&mut self.output, token, deadline)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_response(
    output: &mut ChildStdout,
    token: &CancelToken,
    deadline: Instant,
) -> io::Result<Response> {
    struct Timed<'a> {
        output: &'a mut ChildStdout,
        token: &'a CancelToken,
        deadline: Instant,
    }
    impl Read for Timed<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let timeout = Timespec {
                tv_sec: 0,
                tv_nsec: 25_000_000,
            };
            loop {
                if self.token.is_terminal() {
                    return Err(io::Error::other("host request cancelled"));
                }
                if Instant::now() >= self.deadline {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                match poll(
                    &mut [PollFd::new(self.output, PollFlags::IN)],
                    Some(&timeout),
                ) {
                    Ok(0) | Err(rustix::io::Errno::INTR) => {}
                    Ok(_) => return self.output.read(buf),
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }
    read_packet(
        &mut Timed {
            output,
            token,
            deadline,
        },
        RESPONSE_LIMIT,
    )?
    .ok_or_else(|| io::ErrorKind::UnexpectedEof.into())
}

fn read_packet<T: DeserializeOwned>(input: &mut impl Read, limit: usize) -> io::Result<Option<T>> {
    let mut header = [0_u8; 4];
    loop {
        match input.read(&mut header[..1]) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            read => {
                if read? == 0 {
                    return Ok(None);
                }
                break;
            }
        }
    }
    input.read_exact(&mut header[1..])?;
    let size = u32::from_le_bytes(header) as usize;
    if size == 0 || size > limit {
        return Err(invalid("host packet out of bounds"));
    }
    let mut payload = vec![0; size];
    input.read_exact(&mut payload)?;
    serde_json::from_slice(&payload)
        .map(Some)
        .map_err(io::Error::from)
}

fn write_packet(output: &mut impl Write, value: &impl Serialize, limit: usize) -> io::Result<()> {
    struct Bounded(Vec<u8>, usize);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.1.saturating_sub(self.0.len()) {
                return Err(invalid("host packet out of bounds"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut payload = Bounded(Vec::new(), limit);
    serde_json::to_writer(&mut payload, value)?;
    let size = u32::try_from(payload.0.len()).map_err(io::Error::other)?;
    output.write_all(&size.to_le_bytes())?;
    output.write_all(&payload.0)?;
    output.flush()
}

#[derive(Default)]
struct Sampler {
    client: Option<Client>,
    retry_at: Option<Instant>,
}

thread_local! {
    static SAMPLER: RefCell<Sampler> = RefCell::new(Sampler::default());
}

pub(crate) fn sample(token: &CancelToken) -> Option<Sample> {
    SAMPLER.with(|state| {
        let mut state = state.borrow_mut();
        if token.is_terminal() || state.retry_at.is_some_and(|retry| retry > Instant::now()) {
            return None;
        }
        let result = match state.client.as_mut() {
            Some(client) => client.call(Request::Sample, token),
            None => Client::open().and_then(|mut client| {
                let response = client.call(Request::Sample, token)?;
                state.client = Some(client);
                Ok(response)
            }),
        };
        if let Ok(Response::Sample(sample)) = result {
            return Some(sample);
        }
        state.client = None;
        if !token.is_terminal() {
            log::warn!("Host process collector unavailable: {:?}", result.err());
            state.retry_at = Some(Instant::now() + RETRY_DELAY);
        }
        None
    })
}

pub(crate) fn details(target: &ProcessTarget, token: &CancelToken) -> Reading {
    let reply =
        Client::open().and_then(|mut client| client.call(Request::Details(target.clone()), token));
    match reply {
        Ok(Response::Details(reading)) => reading,
        _ if token.is_terminal() => Reading::failed(Failure::Cancelled),
        Err(error) if error.kind() == io::ErrorKind::TimedOut => Reading::failed(Failure::TimedOut),
        _ => Reading::failed(Failure::Worker),
    }
}

pub(crate) struct HostSignalBackend;

struct HostPin {
    identity: PinIdentity,
    connection: Mutex<PinConnection>,
}

enum PinConnection {
    Live(Client),
    Exited,
    Unavailable,
}

fn unavailable() -> SignalFailure {
    SignalFailure::Other("host process worker unavailable".into())
}

impl ProcessSignalBackend for HostSignalBackend {
    fn pin(&self, pid: u32) -> Result<Box<dyn PinnedProcess>, SignalFailure> {
        let mut client = Client::open().map_err(|_| unavailable())?;
        let Response::Pinned(identity) = client
            .call(Request::Pin(pid), &CancelToken::new())
            .map_err(|_| unavailable())?
        else {
            return Err(unavailable());
        };
        Ok(Box::new(HostPin {
            identity: identity?,
            connection: Mutex::new(PinConnection::Live(client)),
        }))
    }
}

impl PinnedProcess for HostPin {
    fn start_time_ticks(&self) -> Result<u64, SignalFailure> {
        Ok(self.identity.start)
    }

    fn uid(&self) -> Result<u32, SignalFailure> {
        Ok(self.identity.uid)
    }

    fn send_signal(&self, signal: Signal) -> Result<(), SignalFailure> {
        match self.call(Request::Send(signal))? {
            Response::Sent(result) => result,
            _ => Err(unavailable()),
        }
    }

    fn has_exited(&self) -> Result<bool, SignalFailure> {
        match self.call(Request::Exited)? {
            Response::Exited(result) => result,
            _ => Err(unavailable()),
        }
    }
}

impl HostPin {
    fn call(&self, request: Request) -> Result<Response, SignalFailure> {
        let mut connection = self.connection.lock().map_err(|_| unavailable())?;
        let client = match &mut *connection {
            PinConnection::Live(client) => client,
            PinConnection::Exited if matches!(request, Request::Exited) => {
                return Ok(Response::Exited(Ok(true)));
            }
            PinConnection::Exited => return Err(SignalFailure::Gone),
            PinConnection::Unavailable => return Err(unavailable()),
        };
        let result = client.call(request, &CancelToken::new());
        match result {
            Ok(Response::Exited(Ok(true))) => *connection = PinConnection::Exited,
            Err(_) => *connection = PinConnection::Unavailable,
            _ => {}
        }
        result.map_err(|_| unavailable())
    }
}

impl Worker {
    fn dispatch(&mut self, request: Request) -> Response {
        match request {
            Request::Sample => {
                let (rows, _) = crate::services::collect_processes_local(&CancelToken::new());
                Response::Sample(Sample {
                    rows,
                    threads: crate::services::collect_thread_total(),
                })
            }
            Request::Details(target) => {
                Response::Details(process_details::collect_local(&target, &CancelToken::new()))
            }
            Request::Pin(pid) => Response::Pinned(self.pin(pid)),
            Request::Send(signal) => Response::Sent(self.send(signal)),
            Request::Exited => Response::Exited(
                self.pin
                    .as_ref()
                    .ok_or(SignalFailure::IdentityUnverifiable)
                    .and_then(|(pin, _)| pin.has_exited()),
            ),
        }
    }

    fn pin(&mut self, pid: u32) -> Result<PinIdentity, SignalFailure> {
        self.pin = None;
        if pid <= 1 || pid == std::process::id() || !crate::services::local_signal_target(pid) {
            return Err(SignalFailure::NotPermitted);
        }
        let pin = LinuxProcessSignalBackend.pin(pid)?;
        let start = pin.start_time_ticks()?;
        let uid = pin.uid()?;
        self.pin = Some((pin, PinIdentity { start, uid }));
        Ok(PinIdentity { start, uid })
    }

    fn send(&self, signal: Signal) -> Result<(), SignalFailure> {
        let (pin, original) = self
            .pin
            .as_ref()
            .ok_or(SignalFailure::IdentityUnverifiable)?;
        if pin.start_time_ticks()? != original.start {
            return Err(SignalFailure::IdentityChanged);
        }
        if original.uid == 0
            || pin.uid()? != original.uid
            || original.uid != rustix::process::geteuid().as_raw()
        {
            return Err(SignalFailure::NotPermitted);
        }
        pin.send_signal(signal)
    }
}

pub(crate) fn serve() -> io::Result<()> {
    let mut worker = Worker::default();
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    while let Some(request) = read_packet::<Request>(&mut input, REQUEST_LIMIT)? {
        let response = worker.dispatch(request);
        write_packet(&mut output, &response, RESPONSE_LIMIT)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_rejects_oversized_and_truncated_packets() {
        let oversized = (REQUEST_LIMIT as u32 + 1).to_le_bytes();
        assert!(matches!(
            read_packet::<Request>(&mut oversized.as_slice(), REQUEST_LIMIT),
            Err(error) if error.kind() == io::ErrorKind::InvalidData
        ));

        let truncated = 1_u32.to_le_bytes();
        assert!(matches!(
            read_packet::<Request>(&mut truncated.as_slice(), REQUEST_LIMIT),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof
        ));

        let mut output = Vec::new();
        assert!(write_packet(&mut output, &Request::Sample, 1).is_err());
        assert!(output.is_empty());
    }

    #[test]
    fn worker_refuses_to_pin_itself_or_init() {
        let mut worker = Worker::default();
        for pid in [1, std::process::id()] {
            assert!(matches!(
                worker.dispatch(Request::Pin(pid)),
                Response::Pinned(Err(SignalFailure::NotPermitted))
            ));
        }
        assert!(matches!(
            worker.dispatch(Request::Send(Signal::Kill)),
            Response::Sent(Err(SignalFailure::IdentityUnverifiable))
        ));
    }
}
