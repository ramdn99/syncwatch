//! Dev tool (ADR 0012): which byte ranges does mpv need to open a file at a position and to
//! seek? Serves the file over a throttled local HTTP range server (like the torrent loopback
//! server), opens it with `start=` exactly as the app does, and prints every range mpv read in
//! each phase, until mpv shows the frame there.
//!
//! cargo run -p syncwatch-player --example range_probe -- <file> <start_s> <seek_s>
//! (libmpv-2.dll must be found: put LIBMPV_DIR on PATH for this run.)
#![allow(clippy::print_stdout, clippy::unwrap_used, clippy::expect_used)]

use libmpv2::Mpv;
use libmpv2::events::Event;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Throttle: bytes per second per connection (a modest swarm).
const RATE: u64 = 4 * 1024 * 1024;

#[derive(Default)]
struct Log {
    /// (phase, start, bytes sent so far)
    reqs: Vec<(u32, u64, u64)>,
    phase: u32,
}

fn serve(mut s: TcpStream, path: PathBuf, log: Arc<Mutex<Log>>) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut range: Option<(u64, Option<u64>)> = None;
    let mut first = String::new();
    if r.read_line(&mut first).is_err() {
        return;
    }
    loop {
        let mut l = String::new();
        if r.read_line(&mut l).unwrap_or(0) == 0 || l == "\r\n" {
            break;
        }
        let low = l.to_ascii_lowercase();
        if let Some(v) = low.strip_prefix("range: bytes=") {
            let v = v.trim();
            let (a, b) = v.split_once('-').unwrap_or((v, ""));
            range = Some((a.parse().unwrap_or(0), b.parse().ok()));
        }
    }
    let mut f = std::fs::File::open(&path).unwrap();
    let len = f.metadata().unwrap().len();
    let (start, end) = match range {
        Some((a, b)) => (a.min(len), b.map(|b| b + 1).unwrap_or(len).min(len)),
        None => (0, len),
    };
    let head = if range.is_some() {
        format!(
            "HTTP/1.1 206 Partial Content\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
            end - start,
            start,
            end.saturating_sub(1),
            len
        )
    } else {
        format!(
            "HTTP/1.1 200 OK\r\nAccept-Ranges: bytes\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
        )
    };
    if s.write_all(head.as_bytes()).is_err() || first.starts_with("HEAD") {
        return;
    }
    let idx = {
        let mut g = log.lock().unwrap();
        let p = g.phase;
        g.reqs.push((p, start, 0));
        g.reqs.len() - 1
    };
    f.seek(SeekFrom::Start(start)).unwrap();
    let mut pos = start;
    let mut buf = vec![0u8; 64 * 1024];
    let t0 = Instant::now();
    while pos < end {
        let n = ((end - pos) as usize).min(buf.len());
        let n = f.read(&mut buf[..n]).unwrap_or(0);
        if n == 0 || s.write_all(&buf[..n]).is_err() {
            break;
        }
        pos += n as u64;
        log.lock().unwrap().reqs[idx].2 = pos - start;
        // Throttle to RATE.
        let due = Duration::from_secs_f64((pos - start) as f64 / RATE as f64);
        if let Some(w) = due.checked_sub(t0.elapsed()) {
            std::thread::sleep(w);
        }
    }
}

fn wait_restart(mpv: &Mpv, what: &str, t0: Instant) {
    loop {
        match mpv.wait_event(10.0) {
            Some(Ok(Event::PlaybackRestart)) => break,
            Some(Ok(Event::EndFile(r))) => panic!("{what}: end file {r:?}"),
            None if t0.elapsed() > Duration::from_secs(120) => panic!("{what}: timeout"),
            _ => {}
        }
    }
}

fn report(log: &Arc<Mutex<Log>>, phase: u32, len: u64) -> u64 {
    let g = log.lock().unwrap();
    let mut total = 0;
    for (p, s, n) in g.reqs.iter().filter(|r| r.0 == phase) {
        let _ = p;
        let where_ = if *s < 4 * 1024 * 1024 {
            "start of file"
        } else if *s + 16 * 1024 * 1024 >= len {
            "end of file"
        } else {
            "middle"
        };
        println!(
            "    read {:>12} .. {:>12}  ({:>9} bytes, {where_})",
            s,
            s + n,
            n
        );
        total += n;
    }
    total
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let path = PathBuf::from(&a[1]);
    let start: f64 = a[2].parse().unwrap();
    let seek: f64 = a[3].parse().unwrap();
    let len = std::fs::metadata(&path).unwrap().len();
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(Log::default()));
    {
        let (log, path) = (log.clone(), path.clone());
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let (log, path) = (log.clone(), path.clone());
                std::thread::spawn(move || serve(s, path, log));
            }
        });
    }
    let mpv = Mpv::with_initializer(|i| {
        for (k, v) in [
            ("vo", "null"),
            ("ao", "null"),
            ("pause", "yes"),
            ("idle", "yes"),
            ("keep-open", "yes"),
            ("ytdl", "no"),
            ("load-scripts", "no"),
            ("terminal", "no"),
            ("cache-pause", "yes"),
            ("cache-pause-wait", "2"),
            ("cache-pause-initial", "no"),
            ("demuxer-readahead-secs", "20"),
            ("network-timeout", "600"),
        ] {
            i.set_option(k, v)?;
        }
        Ok(())
    })
    .unwrap();
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let url = format!("http://127.0.0.1:{port}/{name}");
    println!(
        "{} ({} bytes): open at {start} s, then seek to {seek} s",
        path.display(),
        len
    );
    let t0 = Instant::now();
    let opt = format!("start={start:.3}");
    mpv.command("loadfile", &[&url, "replace", "-1", &opt])
        .unwrap();
    wait_restart(&mpv, "open", t0);
    let dur: f64 = mpv.get_property("duration").unwrap_or(0.0);
    let pos: f64 = mpv.get_property("time-pos").unwrap_or(-1.0);
    println!(
        "  open: frame at {pos:.2} s of {dur:.0} s after {:?}",
        t0.elapsed()
    );
    let opened = report(&log, 0, len);
    log.lock().unwrap().phase = 1;
    let t1 = Instant::now();
    mpv.command("seek", &[&format!("{seek}"), "absolute"])
        .unwrap();
    wait_restart(&mpv, "seek", t1);
    let pos: f64 = mpv.get_property("time-pos").unwrap_or(-1.0);
    println!("  seek: frame at {pos:.2} s after {:?}", t1.elapsed());
    let seeked = report(&log, 1, len);
    println!("  SUMMARY {name}: open {opened} bytes, seek {seeked} bytes");
}
