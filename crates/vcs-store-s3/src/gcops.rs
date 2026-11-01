//! The three things remote GC needs from S3 besides `DELETE` (docs/gc.md): touch, object age and a
//! trusted clock. All three read the *server's* clock, never this machine's.
//!
//! * `touch` = server-side copy of the object onto itself with `x-amz-metadata-directive: REPLACE`
//!   (the only way to refresh `LastModified` without moving data). The response body carries the new
//!   `LastModified`. Needs `s3:GetObject` + `s3:PutObject`. The ETag of an object written by a single PUT
//!   does not change (it is the MD5 of the data); objects over 5 GiB cannot be copied (packs are far below).
//! * `modified_ms` = `LastModified` from a ListObjectsV2 whose prefix is the exact key (one request, no HEAD
//!   response headers needed).
//! * `now_ms` = a touch of the tiny object `<prefix>gc/clock`, anchored once and then advanced with the
//!   local monotonic clock (re-anchored every [`REANCHOR`]): publishers only need elapsed time between two
//!   readings and the sweeper needs ages on the same clock as `LastModified`, so one request per
//!   ten minutes replaces one per reading.

use super::{uri_encode, BackendError, Resp, S3Backend};
use std::time::{Duration, Instant};

const REANCHOR: Duration = Duration::from_secs(600);
const CLOCK_KEY: &str = "gc/clock";

/// Server time of the last anchor and when (locally, monotonic) it was read.
pub(crate) type Anchor = std::sync::Mutex<Option<(u64, Instant)>>;

impl S3Backend {
    fn copy_onto_self(&self, full_key: &str) -> Result<Option<u64>, BackendError> {
        let src = format!("/{}/{}", uri_encode(&self.cfg.bucket, true), uri_encode(full_key, false));
        let h = [("x-amz-copy-source", src.as_str()), ("x-amz-metadata-directive", "REPLACE")];
        match self.send("PUT", full_key, &[], &h, b"")? {
            Resp::Ok { body, .. } => {
                let text = String::from_utf8_lossy(&body);
                // a 200 can still carry an <Error> body (S3 documents this for copies)
                if text.contains("<Error>") {
                    return Err(BackendError::Unavailable(format!("touch {full_key}: copy returned an error body")));
                }
                let t = tag_text(&text, "LastModified")
                    .and_then(parse_iso_ms)
                    .ok_or_else(|| BackendError::Other("touch: no LastModified in the copy response".into()))?;
                Ok(Some(t))
            }
            Resp::Status(404, _) => Ok(None),
            Resp::Status(c, m) => Err(BackendError::Other(format!("touch {full_key}: HTTP {c} {m}"))),
        }
    }

    pub(crate) fn gc_touch(&self, key: &str) -> Result<bool, BackendError> {
        let k = self.full_key(key)?;
        Ok(self.copy_onto_self(&k)?.is_some())
    }

    pub(crate) fn gc_modified_ms(&self, key: &str) -> Result<Option<u64>, BackendError> {
        let k = self.full_key(key)?;
        let q = [("list-type", "2".to_owned()), ("prefix", k.clone()), ("max-keys", "5".to_owned())];
        let body = match self.send("GET", "", &q, &[], b"")? {
            Resp::Ok { body, .. } => String::from_utf8(body).map_err(|e| BackendError::Other(format!("modified_ms: {e}")))?,
            Resp::Status(c, m) => return Err(BackendError::Other(format!("modified_ms: HTTP {c} {m}"))),
        };
        // keys sharing the prefix (foo, foo.idx) come back too: take the exact one
        for block in body.split("<Contents>").skip(1) {
            if tag_text(block, "Key") == Some(k.as_str()) {
                return tag_text(block, "LastModified")
                    .and_then(parse_iso_ms)
                    .map(Some)
                    .ok_or_else(|| BackendError::Other("modified_ms: unparsable LastModified".into()));
            }
        }
        Ok(None)
    }

    pub(crate) fn gc_now_ms(&self) -> Result<u64, BackendError> {
        {
            let g = self.anchor.lock().unwrap();
            if let Some((server, at)) = *g {
                if at.elapsed() < REANCHOR {
                    return Ok(server + at.elapsed().as_millis() as u64);
                }
            }
        }
        let k = self.full_key(CLOCK_KEY)?;
        let before = Instant::now();
        let server = match self.copy_onto_self(&k)? {
            Some(t) => t,
            None => {
                // first use: create it (create-only; losing a race is fine), then touch
                match self.send("PUT", &k, &[], &[("if-none-match", "*")], b"clock")? {
                    Resp::Ok { .. } | Resp::Status(412 | 409, _) => {}
                    Resp::Status(c, m) => return Err(BackendError::Other(format!("clock object: HTTP {c} {m}"))),
                }
                self.copy_onto_self(&k)?
                    .ok_or_else(|| BackendError::Other("clock object vanished".into()))?
            }
        };
        // The server stamped the copy somewhere inside this round trip. Anchor at the instant the request
        // was sent: the estimate then runs ahead of the server by at most one round trip and never behind a
        // time it already stamped (ages look slightly older, the safe direction for a grace period).
        let at = before;
        *self.anchor.lock().unwrap() = Some((server, at));
        Ok(server + at.elapsed().as_millis() as u64)
    }
}

fn tag_text<'a>(s: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let i = s.find(&open)? + open.len();
    let j = s[i..].find(&format!("</{tag}>"))?;
    Some(&s[i..i + j])
}

/// `2024-05-01T12:00:00.000Z` or `...:00Z` (UTC) to unix milliseconds.
pub fn parse_iso_ms(s: &str) -> Option<u64> {
    let s = s.trim().strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let (y, m, day): (i64, i64, i64) = (d.next()?.parse().ok()?, d.next()?.parse().ok()?, d.next()?.parse().ok()?);
    let (hms, frac) = time.split_once('.').unwrap_or((time, ""));
    let mut t = hms.split(':');
    let (hh, mm, ss): (i64, i64, i64) = (t.next()?.parse().ok()?, t.next()?.parse().ok()?, t.next()?.parse().ok()?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) || hh > 23 || mm > 59 || ss > 60 || y < 1970 {
        return None;
    }
    let ms: i64 = if frac.is_empty() {
        0
    } else {
        let f: String = frac.chars().take(3).collect();
        if !f.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        format!("{f:0<3}").parse().ok()?
    };
    // days from civil (Howard Hinnant)
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(((days * 24 + hh) * 60 + mm) * 60_000 + ss * 1000 + ms).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn iso_parsing() {
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso_ms("2024-02-29T12:34:56.789Z"), Some(1_709_210_096_789));
        assert_eq!(parse_iso_ms("2000-03-01T00:00:00.5Z"), Some(951_868_800_500));
        for bad in ["", "2024-13-01T00:00:00Z", "2024-01-01 00:00:00Z", "2024-01-01T00:00:00", "x"] {
            assert_eq!(parse_iso_ms(bad), None, "{bad}");
        }
    }
}
