//! Two processes over real UDP on 127.0.0.1: this test starts a copy of its
//! own test binary as the server (the ignored `server_process` test, with
//! `RE_NET_LOOPBACK_SERVER` naming a file it writes its port to), joins it as the client, receives a 512 KiB
//! join download, and has 1,000 reliable messages echoed back intact and in
//! order, at 60 Hz in real time.

use std::process::Command;
use std::time::{Duration, Instant};

use re_net::{Channel, Config, Endpoint, Event, UdpTransport};

const TICK: Duration = Duration::from_micros(16_667);
const ENV: &str = "RE_NET_LOOPBACK_SERVER";
const DOWNLOAD: usize = 512;

fn message(i: u32) -> Vec<u8> {
    let len = if i.is_multiple_of(50) {
        6000
    } else {
        40 + (i as usize * 37) % 260
    };
    (0..len).map(|k| (i as usize * 31 + k) as u8).collect()
}

/// Paces a loop to 60 Hz and gives the time since it started.
struct Clock {
    start: Instant,
    next: Instant,
}

impl Clock {
    fn new() -> Clock {
        let now = Instant::now();
        Clock {
            start: now,
            next: now,
        }
    }
    fn tick(&mut self) -> Duration {
        self.next += TICK;
        std::thread::sleep(self.next.saturating_duration_since(Instant::now()));
        self.start.elapsed()
    }
}

/// The server: writes its port to the file `RE_NET_LOOPBACK_SERVER` names, sends each client the download, echoes
/// every reliable message, and exits when its client leaves.
#[test]
#[ignore = "run by `two_processes_over_udp` as the server process"]
fn server_process() {
    let Some(port_file) = std::env::var_os(ENV) else {
        return;
    };
    let transport = UdpTransport::bind(([127, 0, 0, 1], 0).into()).unwrap();
    let mut server = Endpoint::server(transport, Config::server(1, 0xabc));
    let port = server.local_addr().port().to_string();
    std::fs::write(format!("{}.tmp", port_file.to_string_lossy()), port).unwrap();
    std::fs::rename(format!("{}.tmp", port_file.to_string_lossy()), &port_file).unwrap();
    let mut clock = Clock::new();
    let mut joined = false;
    loop {
        let now = clock.tick();
        for e in server.poll(now) {
            match e {
                Event::Connected(p) => {
                    joined = true;
                    for k in 0..DOWNLOAD {
                        server
                            .send(p, Channel::Reliable, &[(k % 251) as u8; 1024])
                            .unwrap();
                    }
                }
                Event::Message { peer, bytes, .. } => {
                    server.send(peer, Channel::Reliable, &bytes).unwrap();
                }
                Event::Disconnected { .. } => {
                    server.flush(now);
                    return;
                }
            }
        }
        server.flush(now);
        assert!(joined || now < Duration::from_secs(20), "nobody joined");
        assert!(now < Duration::from_secs(120), "the client never left");
    }
}

/// Kills the server process if the client side fails first.
struct Reap(std::process::Child);

impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn two_processes_over_udp() {
    let dir = std::env::temp_dir().join(format!("re_net_loopback_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let port_file = dir.join("port");
    // A leftover from an earlier run with the same process id.
    let _ = std::fs::remove_file(&port_file);
    let mut child = Reap(
        Command::new(std::env::current_exe().unwrap())
            .args(["server_process", "--exact", "--ignored", "--test-threads=1"])
            .env(ENV, &port_file)
            .spawn()
            .unwrap(),
    );
    let started = Instant::now();
    let port: u16 = loop {
        if let Ok(text) = std::fs::read_to_string(&port_file) {
            break text.trim().parse().unwrap();
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the server wrote no port"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let server = ([127, 0, 0, 1], port).into();
    let mut client = Endpoint::client(
        UdpTransport::for_server(server).unwrap(),
        server,
        Config::client(1, 0xabc),
    );
    let mut clock = Clock::new();
    let (mut peer, mut downloaded, mut sent, mut echoed) = (None, 0, 0u32, 0u32);
    let n = 1000;
    while echoed < n {
        let now = clock.tick();
        assert!(
            now < Duration::from_secs(60),
            "{downloaded} KiB, {echoed} echoes"
        );
        for e in client.poll(now) {
            match e {
                Event::Connected(p) => peer = Some(p),
                Event::Message { bytes, .. } if downloaded < DOWNLOAD => {
                    assert_eq!(bytes, vec![(downloaded % 251) as u8; 1024]);
                    downloaded += 1;
                }
                Event::Message { bytes, .. } => {
                    assert!(
                        bytes == message(echoed),
                        "echo {echoed} damaged or out of order"
                    );
                    echoed += 1;
                }
                Event::Disconnected { reason, .. } => panic!("{}", reason.render_human()),
            }
        }
        if let Some(p) = peer
            && downloaded == DOWNLOAD
        {
            for _ in 0..20 {
                if sent < n {
                    client.send(p, Channel::Reliable, &message(sent)).unwrap();
                    sent += 1;
                }
            }
        }
        client.flush(now);
    }
    let now = clock.tick();
    client.disconnect_all(now, "done");
    assert!(
        child.0.wait().unwrap().success(),
        "the server process failed"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let stats = client.stats(re_net::PeerId(0));
    assert!(stats.is_none(), "closed");
}
