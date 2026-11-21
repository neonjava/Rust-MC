//! Load test: offline-mode bots that join a running RustMC server, walk,
//! chat and watch each other, then report what they measured.
//!
//! Why: player capacity may only be claimed from measurements; this drives
//! real protocol traffic (login, chunk streaming, movement, entity updates)
//! the way clients do.
//! Done: N bots in parallel, each walking a circle at walking speed and
//! chatting every 10 s; login time, chunks received, other players' updates
//! and disconnects per bot; optional CPU and memory of the server process
//! from `/proc` (Linux).
//! Not done: block building under load, online-mode logins, Bedrock.
//!
//! Usage: `load_test <address> <bots> <seconds> [server-pid]`; the server
//! must run in offline mode with `max_players` of at least `<bots>`.

use std::error::Error;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use rustmc_server::java_packets::{configuration, handshake, intent, login, play};
use rustmc_server::java_wire::{put_string, put_varint, read_varint};
use rustmc_server::net::configuration::known_packs;
use rustmc_server::net::framing::{FrameDecoder, FrameEncoder, Packet};
use rustmc_server::net::identity::offline_uuid;

/// Java 26.3's protocol version.
const PROTOCOL: u32 = 777;
/// Largest packet a bot accepts.
const MAX_PACKET: usize = 1 << 23;
/// Walking speed: 4.317 blocks per second (minecraft.wiki `Walking`), sent
/// once per 50 ms tick.
const WALK_PER_TICK: f64 = 4.317 / 20.0;
/// Radius of each bot's walking circle, in blocks.
const CIRCLE_RADIUS: f64 = 8.0;
/// Seconds between a bot's chat messages.
const CHAT_EVERY: Duration = Duration::from_secs(10);
const TICK: Duration = Duration::from_millis(50);

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// What one bot measured.
#[derive(Debug, Default)]
struct Report {
    login_ms: f64,
    first_batch_ms: Option<f64>,
    chunks: u64,
    others_moved: u64,
    chats_seen: u64,
    /// Teleports the server sent after the join; a walking bot expects none.
    teleports: u64,
    disconnected: Option<String>,
}

struct Connection {
    stream: TcpStream,
    encoder: FrameEncoder,
    decoder: FrameDecoder,
}

impl Connection {
    fn send(&mut self, id: u32, body: &[u8]) -> Result<()> {
        let mut wire = Vec::new();
        self.encoder.encode(id, body, &mut wire)?;
        self.stream.write_all(&wire)?;
        Ok(())
    }

    fn receive(&mut self) -> Result<Packet> {
        loop {
            if let Some(packet) = self.decoder.next_packet()? {
                return Ok(packet);
            }
            let mut buffer = [0u8; 16 * 1024];
            let read = self.stream.read(&mut buffer)?;
            if read == 0 {
                return Err("server closed the connection".into());
            }
            self.decoder.push(&buffer[..read])?;
        }
    }
}

/// Logs in and configures; returns the connection, the spawn position and
/// the teleport to confirm.
fn join(address: SocketAddr, name: &str) -> Result<(Connection, [f64; 3], u32)> {
    let stream = TcpStream::connect(address)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut connection = Connection {
        stream,
        encoder: FrameEncoder::new(),
        decoder: FrameDecoder::new(MAX_PACKET),
    };
    let mut hello = Vec::new();
    put_varint(PROTOCOL, &mut hello);
    put_string(&address.ip().to_string(), &mut hello);
    hello.extend(address.port().to_be_bytes());
    put_varint(intent::LOGIN, &mut hello);
    connection.send(handshake::serverbound::INTENTION, &hello)?;
    let mut start = Vec::new();
    put_string(name, &mut start);
    start.extend(offline_uuid(name));
    connection.send(login::serverbound::HELLO, &start)?;
    loop {
        let packet = connection.receive()?;
        match packet.id {
            login::clientbound::SET_COMPRESSION => {
                let (threshold, _) = read_varint(&packet.body)
                    .ok()
                    .flatten()
                    .ok_or("bad threshold")?;
                connection.encoder.enable_compression(threshold as usize);
                connection.decoder.enable_compression(threshold as usize);
            }
            login::clientbound::LOGIN_FINISHED => break,
            other => return Err(format!("unexpected login packet {other}").into()),
        }
    }
    connection.send(login::serverbound::LOGIN_ACKNOWLEDGED, &[])?;
    loop {
        let packet = connection.receive()?;
        match packet.id {
            configuration::clientbound::SELECT_KNOWN_PACKS => {
                connection.send(
                    configuration::serverbound::SELECT_KNOWN_PACKS,
                    &known_packs().body,
                )?;
            }
            configuration::clientbound::FINISH_CONFIGURATION => break,
            _ => {}
        }
    }
    connection.send(configuration::serverbound::FINISH_CONFIGURATION, &[])?;
    loop {
        let packet = connection.receive()?;
        if packet.id == play::clientbound::PLAYER_POSITION {
            let (teleport, n) = read_varint(&packet.body)
                .ok()
                .flatten()
                .ok_or("bad teleport")?;
            let mut position = [0.0; 3];
            for (axis, value) in position.iter_mut().enumerate() {
                let at = n + axis * 8;
                let bytes = packet.body.get(at..at + 8).ok_or("short teleport")?;
                *value = f64::from_be_bytes(bytes.try_into()?);
            }
            return Ok((connection, position, teleport));
        }
    }
}

fn run_bot(
    address: SocketAddr,
    index: usize,
    bots: usize,
    seconds: u64,
    stop: Arc<AtomicBool>,
) -> Report {
    let mut report = Report::default();
    let started = Instant::now();
    let name = format!("Bot{index}");
    let (mut connection, spawn, teleport) = match join(address, &name) {
        Ok(joined) => joined,
        Err(error) => {
            report.disconnected = Some(format!("login: {error}"));
            return report;
        }
    };
    report.login_ms = started.elapsed().as_secs_f64() * 1_000.0;
    let mut confirm = Vec::new();
    put_varint(teleport, &mut confirm);
    let result = (|| -> Result<()> {
        connection.send(play::serverbound::ACCEPT_TELEPORTATION, &confirm)?;
        let mut writer = Connection {
            stream: connection.stream.try_clone()?,
            encoder: std::mem::replace(&mut connection.encoder, FrameEncoder::new()),
            decoder: FrameDecoder::new(MAX_PACKET),
        };
        let (outgoing, queue) = mpsc::channel::<(u32, Vec<u8>)>();
        let walker = {
            let outgoing = outgoing.clone();
            let stop = Arc::clone(&stop);
            thread::spawn(move || walk(spawn, index, bots, seconds, &outgoing, &stop))
        };
        let sender = thread::spawn(move || -> Result<()> {
            for (id, body) in queue {
                writer.send(id, &body)?;
            }
            Ok(())
        });
        let deadline = Instant::now() + Duration::from_secs(seconds);
        while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
            let packet = connection.receive()?;
            match packet.id {
                play::clientbound::KEEP_ALIVE => {
                    let _ = outgoing.send((play::serverbound::KEEP_ALIVE, packet.body));
                }
                play::clientbound::CHUNK_BATCH_FINISHED => {
                    report
                        .first_batch_ms
                        .get_or_insert(started.elapsed().as_secs_f64() * 1_000.0);
                    let _ = outgoing.send((
                        play::serverbound::CHUNK_BATCH_RECEIVED,
                        64.0f32.to_be_bytes().to_vec(),
                    ));
                }
                play::clientbound::LEVEL_CHUNK_WITH_LIGHT => report.chunks += 1,
                play::clientbound::ENTITY_POSITION_SYNC => report.others_moved += 1,
                play::clientbound::SYSTEM_CHAT => report.chats_seen += 1,
                play::clientbound::PLAYER_POSITION => {
                    // Confirmed like a client does, and counted: walking
                    // within the movement rules should cause none.
                    report.teleports += 1;
                    if let Ok(Some((teleport, _))) = read_varint(&packet.body) {
                        let mut confirm = Vec::new();
                        put_varint(teleport, &mut confirm);
                        let _ = outgoing.send((play::serverbound::ACCEPT_TELEPORTATION, confirm));
                    }
                }
                play::clientbound::DISCONNECT => {
                    return Err("disconnected by the server".into());
                }
                _ => {}
            }
        }
        drop(outgoing);
        let _ = walker.join();
        sender.join().map_err(|_| "sender panicked")??;
        Ok(())
    })();
    if let Err(error) = result {
        report.disconnected = Some(error.to_string());
    }
    report
}

/// Walks a circle around the spawn, each bot starting at its own angle,
/// chatting now and then.
fn walk(
    spawn: [f64; 3],
    index: usize,
    bots: usize,
    seconds: u64,
    outgoing: &mpsc::Sender<(u32, Vec<u8>)>,
    stop: &AtomicBool,
) {
    let centre = spawn;
    let step = WALK_PER_TICK / CIRCLE_RADIUS;
    let started = Instant::now();
    let mut last_chat = started;
    let mut angle = std::f64::consts::TAU * index as f64 / bots.max(1) as f64;
    let mut next = started;
    while started.elapsed() < Duration::from_secs(seconds) && !stop.load(Ordering::Relaxed) {
        angle += step;
        let mut body = Vec::new();
        for value in [
            centre[0] + CIRCLE_RADIUS * angle.cos(),
            centre[1],
            centre[2] + CIRCLE_RADIUS * angle.sin(),
        ] {
            body.extend(value.to_be_bytes());
        }
        body.push(1);
        if outgoing
            .send((play::serverbound::MOVE_PLAYER_POS, body))
            .is_err()
        {
            return;
        }
        if last_chat.elapsed() >= CHAT_EVERY {
            last_chat = Instant::now();
            let mut chat = Vec::new();
            put_string("load test", &mut chat);
            chat.extend(0i64.to_be_bytes()); // timestamp
            chat.extend(0i64.to_be_bytes()); // salt
            chat.push(0); // unsigned
            put_varint(0, &mut chat); // message count
            chat.extend([0, 0, 0]); // acknowledged
            chat.push(0); // checksum
            if outgoing.send((play::serverbound::CHAT, chat)).is_err() {
                return;
            }
        }
        next += TICK;
        thread::sleep(next.saturating_duration_since(Instant::now()));
    }
}

/// CPU seconds and resident memory of a process, from `/proc`.
fn process_usage(pid: u32) -> Option<(f64, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<&str> = stat.rsplit_once(") ")?.1.split_whitespace().collect();
    // utime and stime are fields 14 and 15 of the whole line, counted after
    // the command name: indices 11 and 12 here, in clock ticks.
    let ticks: f64 = fields.get(11)?.parse::<f64>().ok()? + fields.get(12)?.parse::<f64>().ok()?;
    // The kernel's USER_HZ, 100 on Linux.
    const CLOCK_TICKS: f64 = 100.0;
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let rss_kib = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some((ticks / CLOCK_TICKS, rss_kib))
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted[index]
}

fn main() -> std::result::Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();
    let (Some(address), Some(bots), Some(seconds)) = (args.get(1), args.get(2), args.get(3)) else {
        return Err("usage: load_test <address> <bots> <seconds> [server-pid]".into());
    };
    let address: SocketAddr = address.parse()?;
    let bots: usize = bots.parse()?;
    let seconds: u64 = seconds.parse()?;
    let pid: Option<u32> = args.get(4).map(|pid| pid.parse()).transpose()?;
    let before = pid.and_then(process_usage);
    let stop = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    let handles: Vec<_> = (0..bots)
        .map(|index| {
            let stop = Arc::clone(&stop);
            thread::spawn(move || run_bot(address, index, bots, seconds, stop))
        })
        .collect();
    let reports: Vec<Report> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap_or_default())
        .collect();
    let elapsed = started.elapsed().as_secs_f64();
    let mut logins: Vec<f64> = reports
        .iter()
        .filter(|r| r.disconnected.is_none() || r.login_ms > 0.0)
        .map(|r| r.login_ms)
        .collect();
    logins.sort_by(f64::total_cmp);
    let mut batches: Vec<f64> = reports.iter().filter_map(|r| r.first_batch_ms).collect();
    batches.sort_by(f64::total_cmp);
    let chunks: u64 = reports.iter().map(|r| r.chunks).sum();
    let moved: u64 = reports.iter().map(|r| r.others_moved).sum();
    let chats: u64 = reports.iter().map(|r| r.chats_seen).sum();
    let dropped: Vec<String> = reports
        .iter()
        .enumerate()
        .filter_map(|(i, r)| r.disconnected.as_ref().map(|why| format!("Bot{i}: {why}")))
        .collect();
    println!("bots={bots} seconds={elapsed:.1}");
    println!(
        "login_ms p50={:.1} p95={:.1} max={:.1}",
        percentile(&logins, 0.5),
        percentile(&logins, 0.95),
        percentile(&logins, 1.0)
    );
    println!(
        "first_chunk_batch_ms p50={:.1} p95={:.1}",
        percentile(&batches, 0.5),
        percentile(&batches, 0.95)
    );
    println!(
        "chunks_received={chunks} per_bot={:.0}",
        chunks as f64 / bots.max(1) as f64
    );
    println!(
        "other_player_updates_per_bot_per_second={:.1}",
        moved as f64 / bots.max(1) as f64 / elapsed
    );
    println!("chat_messages_seen={chats}");
    println!(
        "teleports_after_join={}",
        reports.iter().map(|r| r.teleports).sum::<u64>()
    );
    println!("disconnected={} {}", dropped.len(), dropped.join("; "));
    if let (Some(pid), Some((cpu_before, _))) = (pid, before)
        && let Some((cpu_after, rss_kib)) = process_usage(pid)
    {
        println!(
            "server_cpu_percent_of_one_core={:.1} server_rss_mib={:.1}",
            (cpu_after - cpu_before) / elapsed * 100.0,
            rss_kib as f64 / 1_024.0
        );
    }
    Ok(())
}
