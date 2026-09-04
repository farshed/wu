use std::{
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream},
    thread,
    time::Duration,
};

use anyhow::Context as _;
use release_channel::ReleaseChannel;
use util::ResultExt;

use crate::{OpenListener, RawOpenRequest};

const LOCALHOST: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 1);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(10);
const RECEIVE_TIMEOUT: Duration = Duration::from_millis(35);
const SEND_TIMEOUT: Duration = Duration::from_millis(20);
const FORWARD_TIMEOUT: Duration = Duration::from_millis(250);
const USER_BLOCK: u16 = 100;
// Kept below the per-user ports, which start at 45737 and can reach 65534
// for large user IDs.
const DATA_DIR_PORT_BASE: u16 = 30000;
const DATA_DIR_PORT_RANGE: u16 = 10000;
const MAX_FORWARDED_REQUEST_BYTES: u64 = 1024 * 1024;

fn address() -> SocketAddr {
    // These port numbers are offset by the user ID to avoid conflicts between
    // different users on the same machine. In addition to that the ports for each
    // release channel are spaced out by 100 to avoid conflicts between different
    // users running different release channels on the same machine. This ends up
    // interleaving the ports between different users and different release channels.
    //
    // On macOS user IDs start at 501 and on Linux they start at 1000. The first user
    // on a Mac with ID 501 running a dev channel build will use port 46238, and the
    // second user with ID 502 will use port 46239, and so on. The stable channel
    // uses the next block of ports (46438 for user 501, 46439 for user 502, ...).
    // Wu uses a different port range than Zed so both apps can run side by side
    // without answering each other's single-instance handshake.
    //
    // A custom `--user-data-dir` is a separate instance, so it gets its own port
    // derived from the data directory, below the per-user blocks.
    let port = match *release_channel::RELEASE_CHANNEL {
        ReleaseChannel::Dev => 45737,
        ReleaseChannel::Stable => 45737 + (2 * USER_BLOCK),
    };
    let uid = unsafe { libc::getuid() };
    let user_port = if let Some(data_dir_hash) = paths::custom_data_dir_instance_hash() {
        let hash = data_dir_hash ^ u64::from(uid);
        DATA_DIR_PORT_BASE + (hash % u64::from(DATA_DIR_PORT_RANGE)) as u16
    } else {
        // Ensure that the user ID is not too large to avoid overflow when
        // calculating the port number. This seems unlikely but it doesn't
        // hurt to be safe.
        let max_port = 65535;
        let max_uid: u32 = max_port - port as u32;
        port + (uid % max_uid) as u16
    };

    SocketAddr::V4(SocketAddrV4::new(LOCALHOST, user_port))
}

fn instance_handshake() -> String {
    let handshake = match *release_channel::RELEASE_CHANNEL {
        ReleaseChannel::Dev => "Wu Editor Dev Instance Running",
        ReleaseChannel::Stable => "Wu Editor Stable Instance Running",
    };
    match paths::custom_data_dir_instance_hash() {
        Some(hash) => format!("{handshake} {hash:x}"),
        None => handshake.to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsOnlyInstance {
    Yes,
    No,
}

/// Checks whether another instance is already running. If so, `request` is
/// handed over to it so the paths still get opened. Otherwise this instance
/// starts answering the handshake and opens requests forwarded by later
/// launches through `open_listener`.
pub fn ensure_only_instance(
    open_listener: OpenListener,
    request: RawOpenRequest,
) -> IsOnlyInstance {
    if check_got_handshake(&request) {
        return IsOnlyInstance::No;
    }

    let listener = match TcpListener::bind(address()) {
        Ok(listener) => listener,

        Err(err) => {
            log::warn!("Error binding to single instance port: {err}");
            if check_got_handshake(&request) {
                return IsOnlyInstance::No;
            }

            // Avoid failing to start when some other application by chance already has
            // a claim on the port. This is sub-par as any other instance that gets launched
            // will be unable to communicate with this instance and will duplicate
            log::warn!("Backup handshake request failed, continuing without handshake");
            return IsOnlyInstance::Yes;
        }
    };

    let handshake = instance_handshake();
    if let Err(error) = thread::Builder::new()
        .name("EnsureSingleton".to_string())
        .spawn(move || {
            for stream in listener.incoming() {
                let stream = match stream {
                    Ok(stream) => stream,
                    Err(_) => return,
                };

                // Each connection gets its own thread. Reading the forwarded
                // request can wait up to FORWARD_TIMEOUT, which is longer than
                // a launcher waits for the handshake; serving connections one
                // at a time would let an idle client make a concurrent launch
                // miss the handshake and start a duplicate instance.
                let handshake = handshake.clone();
                let open_listener = open_listener.clone();
                if let Err(error) = thread::Builder::new()
                    .name("SingletonConnection".to_string())
                    .spawn(move || answer_launch(stream, &handshake, &open_listener))
                {
                    log::error!("failed to start single instance connection thread: {error}");
                }
            }
        })
    {
        log::error!("failed to start single instance listener thread: {error}");
    }

    IsOnlyInstance::Yes
}

fn answer_launch(mut stream: TcpStream, handshake: &str, open_listener: &OpenListener) {
    stream.set_nodelay(true).log_err();
    stream.set_write_timeout(Some(SEND_TIMEOUT)).log_err();
    stream.set_read_timeout(Some(FORWARD_TIMEOUT)).log_err();
    if stream.write_all(handshake.as_bytes()).log_err().is_none() {
        return;
    }

    let mut forwarded = Vec::new();
    if (&mut stream)
        .take(MAX_FORWARDED_REQUEST_BYTES)
        .read_to_end(&mut forwarded)
        .log_err()
        .is_none()
        || forwarded.is_empty()
    {
        return;
    }
    if let Some(request) = serde_json::from_slice::<RawOpenRequest>(&forwarded)
        .log_err()
        .filter(|request| !request.is_empty())
    {
        open_listener.open(request);
    }
}

fn check_got_handshake(request: &RawOpenRequest) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&address(), CONNECT_TIMEOUT) else {
        return false;
    };

    let handshake = instance_handshake();
    let mut buf = vec![0u8; handshake.len()];

    stream.set_read_timeout(Some(RECEIVE_TIMEOUT)).log_err();
    if let Err(err) = stream.read_exact(&mut buf) {
        log::warn!("Connected to single instance port but failed to read: {err}");
        return false;
    }

    if buf != handshake.as_bytes() {
        log::warn!("Got wrong instance handshake value");
        return false;
    }

    log::info!("Got instance handshake");
    if !request.is_empty() {
        forward_request(&mut stream, request)
            .context("forwarding open request to the running instance")
            .log_err();
    }
    true
}

fn forward_request(stream: &mut TcpStream, request: &RawOpenRequest) -> anyhow::Result<()> {
    stream.set_write_timeout(Some(FORWARD_TIMEOUT))?;
    stream.write_all(&serde_json::to_vec(request)?)?;
    stream.shutdown(std::net::Shutdown::Write)?;
    Ok(())
}
