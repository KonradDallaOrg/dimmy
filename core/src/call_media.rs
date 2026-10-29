//! Is a call still connected? The question the Mac host asks before it
//! ends an auto-recorded meeting because the call app let go of its audio.
//!
//! A call app stops all audio IO in two situations that look identical from
//! CoreAudio: the user hung up, or the user changed headset (a Jabra
//! reconnecting, AirPods taking over, Teams moving to its certified
//! device). Ending the meeting on the second one stopped Francesco's
//! recording and auto-started a new one on 2026-10-28. What tells them
//! apart is the network: switching headset leaves the call's media stream
//! (RTP over connected UDP) open; hanging up closes it.
//!
//! This is the signal TranscribeX confirms a meeting with (reverse-
//! engineered 2026-10-22): connected UDP sockets of the meeting app that
//! are not DNS (53), mDNS (5353) or HTTPS/QUIC (443), read with `lsof`.
//! The app is identified by its `.app` bundle, not a PID: Teams, Chrome
//! and Zoom keep media and network in helper processes that can restart.

/// Ports that are never a call's media stream.
const NOT_MEDIA_PORTS: [u16; 3] = [53, 443, 5353];

/// Without network evidence, how long an app must stay off audio before
/// the call counts as over. Granola waits 15 minutes of silence; a headset
/// switch is over in a second or two, so 90 s is far on the safe side.
pub const NO_EVIDENCE_GRACE_SECS: f64 = 90.0;

/// What to do with a meeting whose call app released its audio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EndVerdict {
    /// The call is still connected: keep recording.
    Stay,
    /// The call is over.
    End,
    /// No evidence either way yet: ask again in this many seconds.
    RecheckIn(f64),
}

/// `media_now`: live media sockets of the call app right now, `None` when
/// they could not be read. `saw_media`: whether this call ever showed one,
/// i.e. whether "zero" means "closed" or just "not visible on this network"
/// (Teams over TCP/443 behind a corporate proxy).
pub fn end_verdict(
    released_for_secs: f64,
    media_now: Option<usize>,
    saw_media: bool,
) -> EndVerdict {
    assert!(
        released_for_secs.is_finite() && released_for_secs >= 0.0,
        "released_for_secs must be a non-negative duration"
    );
    match media_now {
        Some(n) if n > 0 => EndVerdict::Stay,
        Some(_) if saw_media => EndVerdict::End,
        _ if released_for_secs >= NO_EVIDENCE_GRACE_SECS => EndVerdict::End,
        _ => EndVerdict::RecheckIn(NO_EVIDENCE_GRACE_SECS - released_for_secs),
    }
}

/// `/Applications/Microsoft Teams.app/Contents/Helpers/…/MSTeams Helper` →
/// `/Applications/Microsoft Teams.app`: the outermost bundle, so a helper
/// nested in Frameworks/ belongs to the same app as its parent.
pub fn app_bundle_root(exe_path: &str) -> Option<&str> {
    let end = exe_path.find(".app/")? + ".app".len();
    Some(&exe_path[..end])
}

/// Count connected UDP sockets in `lsof -nP -i UDP` output that can be a
/// call's media stream. A connected socket has a `local->remote` NAME;
/// either endpoint on a DNS, mDNS or HTTPS/QUIC port disqualifies it.
pub fn count_media_sockets(lsof_output: &str) -> usize {
    lsof_output
        .lines()
        .filter_map(|line| line.split_whitespace().find(|f| f.contains("->")))
        .filter(|name| {
            let mut ends = name.split("->");
            match (ends.next().and_then(port_of), ends.next().and_then(port_of)) {
                (Some(local), Some(remote)) => {
                    !NOT_MEDIA_PORTS.contains(&local) && !NOT_MEDIA_PORTS.contains(&remote)
                }
                _ => false,
            }
        })
        .count()
}

/// The port of an `lsof -P` endpoint: `192.168.1.5:50432`, `[::1]:3478`.
fn port_of(endpoint: &str) -> Option<u16> {
    endpoint.rsplit_once(':')?.1.parse().ok()
}

/// Live media sockets of the app `pid` belongs to, across all its
/// processes. `None` when that cannot be read: not macOS, no bundle path,
/// `lsof` missing, failing or slower than 5 s.
pub fn live_media_sockets(pid: i32) -> Option<usize> {
    assert!(pid > 0, "pid must be positive");
    imp::live_media_sockets(pid)
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{app_bundle_root, count_media_sockets};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const LSOF_TIMEOUT: Duration = Duration::from_secs(5);

    pub fn live_media_sockets(pid: i32) -> Option<usize> {
        let path = exe_path(pid)?;
        let root = app_bundle_root(&path)?.to_string();
        let pids = family_pids(&root);
        if pids.is_empty() {
            return None;
        }
        let list = pids
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(",");
        // -a ANDs the selectors: UDP sockets of these processes only.
        let mut child = Command::new("/usr/sbin/lsof")
            .args(["-nP", "-a", "-i", "UDP", "-p", &list])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if started.elapsed() < LSOF_TIMEOUT => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
        // lsof exits 1 when it found nothing to list: that is an answer.
        let out = child.wait_with_output().ok()?;
        Some(count_media_sockets(&String::from_utf8_lossy(&out.stdout)))
    }

    fn exe_path(pid: i32) -> Option<String> {
        let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
        if n <= 0 {
            return None;
        }
        buf.truncate(n as usize);
        String::from_utf8(buf).ok()
    }

    /// Every process whose executable lives inside `root`.
    fn family_pids(root: &str) -> Vec<i32> {
        let prefix = format!("{root}/");
        let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if count <= 0 {
            return Vec::new();
        }
        // Room for processes started between the two calls.
        let mut pids = vec![0i32; count as usize + 64];
        let bytes = (pids.len() * std::mem::size_of::<i32>()) as libc::c_int;
        let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        if n <= 0 {
            return Vec::new();
        }
        pids.truncate(n as usize);
        pids.into_iter()
            .filter(|&p| p > 0 && exe_path(p).is_some_and(|path| path.starts_with(&prefix)))
            .collect()
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn live_media_sockets(_pid: i32) -> Option<usize> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `lsof -nP -a -i UDP -p …` lines as macOS prints them.
    const TEAMS_IN_CALL: &str = "\
COMMAND     PID  USER   FD   TYPE             DEVICE SIZE/OFF NODE NAME
MSTeams    4021 franc   45u  IPv4 0x1a2b3c4d5e6f7a8b      0t0  UDP 192.168.1.20:50432->52.114.88.12:3478
MSTeams    4021 franc   46u  IPv4 0x1a2b3c4d5e6f7a8c      0t0  UDP 192.168.1.20:50433->52.114.88.12:3480
MSTeams    4021 franc   47u  IPv4 0x1a2b3c4d5e6f7a8d      0t0  UDP *:5353
MSTeams    4021 franc   48u  IPv4 0x1a2b3c4d5e6f7a8e      0t0  UDP 192.168.1.20:61000->1.1.1.1:53
MSTeams    4021 franc   49u  IPv6 0x1a2b3c4d5e6f7a8f      0t0  UDP [2a02:1::5]:60000->[2603:1063::1]:443
";

    const TEAMS_HUNG_UP: &str = "\
COMMAND     PID  USER   FD   TYPE             DEVICE SIZE/OFF NODE NAME
MSTeams    4021 franc   47u  IPv4 0x1a2b3c4d5e6f7a8d      0t0  UDP *:5353
MSTeams    4021 franc   49u  IPv6 0x1a2b3c4d5e6f7a8f      0t0  UDP [2a02:1::5]:60000->[2603:1063::1]:443
";

    #[test]
    fn a_call_in_progress_has_media_sockets() {
        assert_eq!(count_media_sockets(TEAMS_IN_CALL), 2);
    }

    #[test]
    fn dns_mdns_and_quic_are_not_media() {
        assert_eq!(count_media_sockets(TEAMS_HUNG_UP), 0);
    }

    #[test]
    fn ipv6_media_counts() {
        let line = "zoom.us 900 u 30u IPv6 0x1 0t0 UDP [2a02:1::5]:51000->[2a03:2::7]:8801\n";
        assert_eq!(count_media_sockets(line), 1);
    }

    #[test]
    fn empty_or_garbage_output_is_zero() {
        assert_eq!(count_media_sockets(""), 0);
        assert_eq!(count_media_sockets("lsof: no such file\n->\nx->y\n"), 0);
    }

    #[test]
    fn helpers_belong_to_the_outer_app() {
        assert_eq!(
            app_bundle_root(
                "/Applications/Microsoft Teams.app/Contents/Helpers/Microsoft Teams WebView.app/Contents/MacOS/Microsoft Teams WebView"
            ),
            Some("/Applications/Microsoft Teams.app")
        );
        assert_eq!(
            app_bundle_root("/Applications/zoom.us.app/Contents/MacOS/zoom.us"),
            Some("/Applications/zoom.us.app")
        );
        assert_eq!(app_bundle_root("/usr/bin/afplay"), None);
    }

    // The headset switch: Teams let go of the audio, the media stream is
    // still up. This verdict is the whole fix.
    #[test]
    fn released_audio_with_live_media_stays() {
        assert_eq!(end_verdict(0.0, Some(2), true), EndVerdict::Stay);
        assert_eq!(end_verdict(600.0, Some(1), false), EndVerdict::Stay);
    }

    #[test]
    fn media_closed_after_being_seen_ends_at_once() {
        assert_eq!(end_verdict(0.0, Some(0), true), EndVerdict::End);
    }

    // Zero sockets on a call that never showed any is no evidence: the
    // network may carry it over TCP/443.
    #[test]
    fn no_evidence_waits_for_the_grace_then_ends() {
        assert_eq!(
            end_verdict(0.0, Some(0), false),
            EndVerdict::RecheckIn(NO_EVIDENCE_GRACE_SECS)
        );
        assert_eq!(
            end_verdict(30.0, None, true),
            EndVerdict::RecheckIn(NO_EVIDENCE_GRACE_SECS - 30.0)
        );
        assert_eq!(
            end_verdict(NO_EVIDENCE_GRACE_SECS, None, false),
            EndVerdict::End
        );
        assert_eq!(
            end_verdict(NO_EVIDENCE_GRACE_SECS + 1.0, Some(0), false),
            EndVerdict::End
        );
    }

    #[test]
    fn grace_is_longer_than_any_headset_switch() {
        assert!(NO_EVIDENCE_GRACE_SECS >= 60.0);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn elsewhere_the_network_is_unknown() {
        assert_eq!(live_media_sockets(std::process::id() as i32), None);
    }
}
