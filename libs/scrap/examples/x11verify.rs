// Does the damage-tracked picture match a fresh full grab? A difference that is gone
// after a moment's more capturing is the compositor still showing a report; one that
// stays is a change the capturer lost.
use scrap::x11::{Capturer, Server};
use std::time::{Duration, Instant};

fn main() {
    hbb_common::env_logger::init_from_env(hbb_common::env_logger::Env::default().filter_or("RUST_LOG", "info"));
    let rounds: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(4);
    let secs: u64 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(5);
    let primary = |server| Server::displays(server).find(|d| d.is_default()).expect("display");
    let mut a = Capturer::new(primary(Server::default().unwrap())).unwrap();
    let w = a.display().rect().w as usize;
    for round in 0..rounds {
        let until = Instant::now() + Duration::from_secs(secs);
        let (mut frames, mut idle) = (0, 0);
        while Instant::now() < until {
            match a.frame(Duration::from_millis(8)) {
                Ok(_) => frames += 1,
                Err(_) => idle += 1,
            }
        }
        // Ground truth: a brand-new capturer's first call is a forced full grab.
        let mut b = Capturer::new(primary(Server::default().unwrap())).unwrap();
        let truth = b.frame(Duration::from_millis(8)).unwrap().to_vec();
        // Let A take in anything reported in between, then compare.
        let _ = a.frame(Duration::from_millis(8));
        let (diff_px, rows) = differ(a.current(), &truth, w);
        let mut later = String::new();
        if diff_px > 0 {
            let until = Instant::now() + Duration::from_millis(700);
            while Instant::now() < until {
                let _ = a.frame(Duration::from_millis(8));
            }
            // Against a fresh grab again: the screen may have moved on meanwhile.
            let mut c = Capturer::new(primary(Server::default().unwrap())).unwrap();
            let truth = c.frame(Duration::from_millis(8)).unwrap().to_vec();
            let _ = a.frame(Duration::from_millis(8));
            let (still, _) = differ(a.current(), &truth, w);
            later = format!("; {still} differ from a fresh grab 700 ms later");
        }
        println!(
            "round {round}: {frames} frames / {idle} idle in {secs}s; differing pixels vs fresh grab: {diff_px} ({} rows{}){later}",
            rows.len(),
            rows.iter().next().map(|r| format!(", first row {r}, last row {}", rows.iter().next_back().unwrap())).unwrap_or_default()
        );
    }
}

fn differ(mine: &[u8], truth: &[u8], w: usize) -> (usize, std::collections::BTreeSet<usize>) {
    let rows: std::collections::BTreeSet<usize> = mine
        .chunks(4).zip(truth.chunks(4)).enumerate()
        .filter(|(_, (x, y))| x != y).map(|(i, _)| i / w).collect();
    let diff_px = mine.chunks(4).zip(truth.chunks(4)).filter(|(x, y)| x != y).count();
    (diff_px, rows)
}
