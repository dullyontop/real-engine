//! Server keys and their fingerprints, join codes, and the random numbers
//! they come from (ADR-0067 §7, §8).
//!
//! Fingerprints and join codes are written in Crockford's base32: the digits
//! and the letters but I, L, O and U. Reading one ignores case, dashes and
//! spaces, and takes I and L as 1 and O as 0, so a code read aloud or copied
//! by hand still works.

use std::fmt;
use std::io::{self, Write};
use std::str::FromStr;

use blake2::{Blake2s256, Digest};
use re_core::Diagnostic;
use re_core::files::WritableDir;
use subtle::ConstantTimeEq;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, Zeroizing};

use crate::codes;

/// Crockford's base32 alphabet.
const SYMBOLS: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Where a server keeps its key, in its data folder
/// ([`Writable::Data`](re_core::files::Writable::Data)).
pub const KEY_FILE: &str = "net/server.key";

/// Fills `out` from the operating system's secure random numbers (the page's
/// `crypto.getRandomValues` in a browser). There is no weaker fallback.
///
/// # Errors
///
/// `RE8916` when the platform has none.
pub(crate) fn random(out: &mut [u8]) -> Result<(), Diagnostic> {
    getrandom::fill(out).map_err(|e| {
        Diagnostic::error(
            codes::SECRETS,
            format!("this platform gives no secure random numbers ({e}), which online play needs for its keys"),
        )
        .with_hint("every supported desktop system and browser has them, so this one's random number source failed")
    })
}

/// A random X25519 secret.
pub(crate) fn random_secret() -> Result<StaticSecret, Diagnostic> {
    let mut b = Zeroizing::new([0u8; 32]);
    random(&mut b[..])?;
    Ok(StaticSecret::from(*b))
}

/// A server's long-term X25519 key (ADR-0067 §7). Clients see its public half
/// in the Challenge and its [`Fingerprint`] where the server prints it. Its
/// `Debug` prints only the fingerprint, and the secret wipes itself when
/// dropped.
#[derive(Clone)]
pub struct ServerKey {
    secret: StaticSecret,
    public: [u8; 32],
}

impl ServerKey {
    /// A new key from the platform's secure random numbers.
    ///
    /// # Errors
    ///
    /// `RE8916` when the platform has none.
    pub fn generate() -> Result<ServerKey, Diagnostic> {
        Ok(ServerKey::from_secret(random_secret()?))
    }

    /// The key with this secret (an X25519 private key; tests and tools use
    /// fixed ones).
    pub fn from_bytes(secret: [u8; 32]) -> ServerKey {
        let secret = Zeroizing::new(secret);
        ServerKey::from_secret(StaticSecret::from(*secret))
    }

    fn from_secret(secret: StaticSecret) -> ServerKey {
        let public = PublicKey::from(&secret).to_bytes();
        ServerKey { secret, public }
    }

    /// The public key, which clients encrypt to.
    pub fn public_key(&self) -> [u8; 32] {
        self.public
    }

    /// The public key's fingerprint, which the server prints and clients pin.
    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::of(&self.public)
    }

    pub(crate) fn secret(&self) -> &StaticSecret {
        &self.secret
    }

    /// The key from the server's data folder ([`KEY_FILE`]), or a new one
    /// written there (mode 0600 on Unix) when there is none. The flag is
    /// true for a new key.
    ///
    /// # Errors
    ///
    /// `RE8916`: the file cannot be read or written, does not parse, or the
    /// platform has no secure random numbers.
    pub fn load_or_make(data: &WritableDir) -> Result<(ServerKey, bool), Diagnostic> {
        let at = || match data.path() {
            Some(p) => p.join(KEY_FILE).display().to_string(),
            None => KEY_FILE.to_string(),
        };
        let fail = |what: String| {
            Diagnostic::error(codes::SECRETS, what).with_hint(format!(
                "delete {} to make a new key (players who pinned the old fingerprint then refuse the server), or make its folder writable",
                at()
            ))
        };
        // Twice at most: when another server makes the file between this
        // one's read and its write, this one reads that server's key.
        for _ in 0..2 {
            match data.read(KEY_FILE) {
                Ok(Some(bytes)) => {
                    let bytes = Zeroizing::new(bytes);
                    let text = std::str::from_utf8(&bytes)
                        .map_err(|_| fail(format!("the server key file {} is not text", at())))?;
                    let key = ServerKey::from_file(text).map_err(|why| {
                        fail(format!(
                            "the server key file {} does not parse: {why}",
                            at()
                        ))
                    })?;
                    return Ok((key, false));
                }
                Ok(None) => {
                    let key = ServerKey::generate()?;
                    let made =
                        create_private(data, KEY_FILE, key.to_file().as_bytes()).map_err(|e| {
                            fail(format!("cannot write the server key file {}: {e}", at()))
                        })?;
                    if made {
                        return Ok((key, true));
                    }
                }
                Err(e) => {
                    return Err(fail(format!(
                        "cannot read the server key file {}: {e}",
                        at()
                    )));
                }
            }
        }
        Err(fail(format!(
            "the server key file {} keeps changing while it is read",
            at()
        )))
    }

    /// The key file's text: a warning, the fingerprint (for people; reading
    /// checks it), and the secret as 64 hex digits.
    pub(crate) fn to_file(&self) -> Zeroizing<String> {
        let mut text = Zeroizing::new(String::with_capacity(256));
        text.push_str(
            "# Real Engine server key. Whoever has this file can pose as this server, so keep it private.\n# Delete it to make a new key; players who pinned the old fingerprint then refuse the server.\n",
        );
        text.push_str(&format!("fingerprint {}\n", self.fingerprint()));
        text.push_str("private-key ");
        for b in self.secret.as_bytes() {
            text.push(char::from(HEX[usize::from(b >> 4)]));
            text.push(char::from(HEX[usize::from(b & 15)]));
        }
        text.push('\n');
        text
    }

    /// Reads [`ServerKey::to_file`]'s text. Blank lines and lines that start
    /// with `#` are skipped.
    ///
    /// # Errors
    ///
    /// Why the text does not parse. It names lines by number and never
    /// quotes them, since a damaged line may hold the secret.
    pub(crate) fn from_file(text: &str) -> Result<ServerKey, String> {
        let mut key = None;
        let mut fingerprint = None;
        for (n, line) in text.lines().map(str::trim).enumerate() {
            let n = n + 1;
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (name, value) = line.split_once(' ').unwrap_or((line, ""));
            let value = value.trim();
            match name {
                "private-key" if key.is_none() => {
                    let mut secret = Zeroizing::new([0u8; 32]);
                    if !unhex(value, &mut secret[..]) {
                        return Err(format!("its private-key (line {n}) is not 64 hex digits"));
                    }
                    key = Some(ServerKey::from_bytes(*secret));
                }
                "fingerprint" if fingerprint.is_none() => {
                    fingerprint = Some(
                        value
                            .parse::<Fingerprint>()
                            .map_err(|_| format!("its fingerprint (line {n}) does not parse"))?,
                    );
                }
                "private-key" | "fingerprint" => {
                    return Err(format!("it has a second `{name}` line (line {n})"));
                }
                _ => return Err(format!("its line {n} is not part of a key file")),
            }
        }
        let key = key.ok_or("it has no private-key line")?;
        match fingerprint {
            Some(f) if f != key.fingerprint() => Err(format!(
                "its fingerprint line says {f}, but its key's fingerprint is {}",
                key.fingerprint()
            )),
            _ => Ok(key),
        }
    }
}

impl PartialEq for ServerKey {
    /// Keys are the same when their public halves are.
    fn eq(&self, other: &ServerKey) -> bool {
        self.public == other.public
    }
}

impl Eq for ServerKey {}

impl fmt::Debug for ServerKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ServerKey({})", self.fingerprint())
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Reads `out.len()` bytes as hex digits (either case) from `text`, which must
/// hold exactly that many; false otherwise.
fn unhex(text: &str, out: &mut [u8]) -> bool {
    let digits = text.as_bytes();
    if digits.len() != out.len() * 2 {
        return false;
    }
    for (o, pair) in out.iter_mut().zip(digits.chunks(2)) {
        let mut v = 0u8;
        for &d in pair {
            let n = match d {
                b'0'..=b'9' => d - b'0',
                b'a'..=b'f' => d - b'a' + 10,
                b'A'..=b'F' => d - b'A' + 10,
                _ => return false,
            };
            v = v << 4 | n;
        }
        *o = v;
    }
    true
}

/// Writes `bytes` to `rel` in `dir` unless the file exists; true when this
/// call made it. On disk the file is written whole under another name and
/// then linked into place, so a crash leaves no half-written key and two
/// servers that start at once end with one key; on Unix only its owner can
/// read it.
fn create_private(dir: &WritableDir, rel: &str, bytes: &[u8]) -> io::Result<bool> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let Some(root) = dir.path() else {
        if dir.read(rel)?.is_some() {
            return Ok(false);
        }
        dir.write(rel, bytes)?;
        return Ok(true);
    };
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp = path.clone().into_os_string();
    tmp.push(format!(
        ".{}.{}.tmp",
        re_core::process_id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = std::path::PathBuf::from(tmp);
    // Only a process that died with this one's id can have left this name.
    let _ = std::fs::remove_file(&tmp);
    let made = match write_new(&tmp, bytes) {
        Ok(true) => match std::fs::hard_link(&tmp, &path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            // A file system without hard links: the file itself is made
            // instead, which still only one server can do.
            Err(_) => write_new(&path, bytes),
        },
        Ok(false) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "another file is in the way of its temporary file",
        )),
        Err(e) => Err(e),
    };
    let _ = std::fs::remove_file(&tmp);
    made
}

/// Writes a new file at `path` that only its owner can read on Unix; false
/// when the file exists.
fn write_new(path: &std::path::Path, bytes: &[u8]) -> io::Result<bool> {
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
    match open.open(path) {
        Ok(mut f) => {
            f.write_all(bytes)?;
            f.sync_all()?;
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

/// The symbols of `text` as Crockford's base32 reads them, as their values
/// (0 to 31); `None` at a character that is none.
fn read_symbols(text: &str) -> Option<Zeroizing<Vec<u8>>> {
    let mut out = Zeroizing::new(Vec::with_capacity(text.len()));
    for c in text.chars() {
        let c = match c.to_ascii_uppercase() {
            '-' | ' ' => continue,
            'I' | 'L' => '1',
            'O' => '0',
            c => c,
        };
        let value = SYMBOLS.iter().position(|&s| char::from(s) == c)?;
        out.push(value as u8);
    }
    Some(out)
}

/// The first 80 bits of BLAKE2s-256(`real-engine/server-key/1` ‖ public key),
/// which name a server key for people (ADR-0067 §7): 16 symbols of Crockford's
/// base32 in four groups, such as `3QF7-M2XK-9TBA-6RHE`. A client given one
/// (`--server-key`) refuses a server whose key has another (`RE8914`).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint([u8; 10]);

impl Fingerprint {
    /// The fingerprint of an X25519 public key.
    pub fn of(public_key: &[u8; 32]) -> Fingerprint {
        let mut h = Blake2s256::new();
        h.update(b"real-engine/server-key/1");
        h.update(public_key);
        let h = h.finalize();
        let mut f = [0u8; 10];
        f.copy_from_slice(&h[..10]);
        Fingerprint(f)
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bits = self.0.iter().fold(0u128, |a, &b| a << 8 | u128::from(b));
        for i in 0..16 {
            if i > 0 && i % 4 == 0 {
                f.write_str("-")?;
            }
            let v = (bits >> (75 - 5 * i)) & 31;
            write!(f, "{}", char::from(SYMBOLS[v as usize]))?;
        }
        Ok(())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({self})")
    }
}

impl FromStr for Fingerprint {
    type Err = Diagnostic;

    /// Reads 16 symbols (dashes, spaces and case aside).
    fn from_str(text: &str) -> Result<Fingerprint, Diagnostic> {
        let bad = || {
            Diagnostic::error(
                codes::BAD_CODE,
                format!("`{text}` is not a server key fingerprint, which is 16 letters and digits such as 3QF7-M2XK-9TBA-6RHE"),
            )
            .with_hint("copy the fingerprint the server printed when it started (the line `server key ...`)")
        };
        let symbols = read_symbols(text)
            .filter(|s| s.len() == 16)
            .ok_or_else(bad)?;
        let bits = symbols.iter().fold(0u128, |a, &v| a << 5 | u128::from(v));
        let mut f = [0u8; 10];
        for (i, b) in f.iter_mut().enumerate() {
            *b = (bits >> (72 - 8 * i)) as u8;
        }
        Ok(Fingerprint(f))
    }
}

/// A join code (ADR-0067 §8): 10 symbols of Crockford's base32 (50 random
/// bits), such as `KD4RT-7XWQP`. A server with one takes only clients that
/// give it; it is mixed into the handshake as a pre-shared key, so it never
/// crosses the network. Its `Debug` hides it, and it wipes itself when
/// dropped.
#[derive(Clone)]
pub struct JoinCode([u8; 10]);

impl JoinCode {
    /// A new code from the platform's secure random numbers.
    ///
    /// # Errors
    ///
    /// `RE8916` when the platform has none.
    pub fn generate() -> Result<JoinCode, Diagnostic> {
        let mut b = Zeroizing::new([0u8; 10]);
        random(&mut b[..])?;
        // 256 is a multiple of 32, so each symbol is uniform.
        let mut code = [0u8; 10];
        for (c, r) in code.iter_mut().zip(b.iter()) {
            *c = SYMBOLS[usize::from(r & 31)];
        }
        Ok(JoinCode(code))
    }

    /// The ten symbols, upper case, as the pre-shared key mixes them in.
    fn symbols(&self) -> &[u8; 10] {
        &self.0
    }
}

impl Drop for JoinCode {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl PartialEq for JoinCode {
    fn eq(&self, other: &JoinCode) -> bool {
        self.0.ct_eq(&other.0).into()
    }
}

impl Eq for JoinCode {}

impl fmt::Display for JoinCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, &c) in self.0.iter().enumerate() {
            if i == 5 {
                f.write_str("-")?;
            }
            write!(f, "{}", char::from(c))?;
        }
        Ok(())
    }
}

impl fmt::Debug for JoinCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JoinCode(..)")
    }
}

impl FromStr for JoinCode {
    type Err = Diagnostic;

    /// Reads 10 symbols (dashes, spaces and case aside; I and L as 1, O as 0).
    fn from_str(text: &str) -> Result<JoinCode, Diagnostic> {
        let symbols = read_symbols(text)
            .filter(|s| s.len() == 10)
            .ok_or_else(|| {
                Diagnostic::error(
                codes::BAD_CODE,
                "a join code is 10 letters and digits, such as KD4RT-7XWQP, and this is not one",
            )
            .with_hint("copy the code the host printed; case, dashes and spaces do not matter")
            })?;
        let mut code = [0u8; 10];
        for (c, &v) in code.iter_mut().zip(symbols.iter()) {
            *c = SYMBOLS[usize::from(v)];
        }
        Ok(JoinCode(code))
    }
}

/// The handshake's pre-shared key for a server's public key and its join
/// code, if it has one (ADR-0067 §8): BLAKE2s-256(`real-engine/join-code/1` ‖
/// public key ‖ the code's symbols). Tying it to the key means one guess tests
/// one code for one server.
pub(crate) fn psk(server: &[u8; 32], code: Option<&JoinCode>) -> Zeroizing<[u8; 32]> {
    let mut h = Blake2s256::new();
    h.update(b"real-engine/join-code/1");
    h.update(server);
    if let Some(code) = code {
        h.update(code.symbols());
    }
    let mut psk = Zeroizing::new([0u8; 32]);
    h.finalize_into((&mut *psk).into());
    psk
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fingerprint_reads_back_however_it_is_written() {
        for seed in 0..50u8 {
            let key = ServerKey::from_bytes([seed; 32]);
            let f = key.fingerprint();
            let text = f.to_string();
            assert_eq!(text.len(), 19, "{text}");
            assert_eq!(text.matches('-').count(), 3);
            assert_eq!(text.parse::<Fingerprint>().unwrap(), f);
            let messy = text
                .replace('-', " ")
                .to_lowercase()
                .replace('1', "l")
                .replace('0', "o");
            assert_eq!(messy.parse::<Fingerprint>().unwrap(), f, "{messy}");
            assert_eq!(text.replace('-', "").parse::<Fingerprint>().unwrap(), f);
        }
    }

    #[test]
    fn a_fingerprint_is_stable() {
        // Pinned: a change here breaks every pinned fingerprint.
        // The values were computed apart from this crate, with Python's
        // `cryptography` and `hashlib`.
        let key = ServerKey::from_bytes([7; 32]);
        assert_eq!(
            hex(&key.public_key()),
            "13be4feaeaf204c7fd3358fc9c00721881d174278128227ec674f37f7fe97b6d"
        );
        assert_eq!(key.fingerprint().to_string(), "BXY0-7PFN-KY0Z-8P8J");
        assert_eq!(format!("{key:?}"), "ServerKey(BXY0-7PFN-KY0Z-8P8J)");
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn bad_fingerprints_and_codes_are_refused() {
        for text in [
            "",
            "3QF7-M2XK-9TBA",
            "3QF7-M2XK-9TBA-6RHE-0",
            "3QF7-M2XK-9TBA-6RHU",
            "3QF7_M2XK_9TBA_6RHE",
            "ÄQF7-M2XK-9TBA-6RHE",
        ] {
            let e = text.parse::<Fingerprint>().unwrap_err();
            assert_eq!(e.code, codes::BAD_CODE, "{text}");
        }
        for text in [
            "",
            "KD4RT-7XWQ",
            "KD4RT-7XWQPP",
            "KD4RT-7XWQU",
            "KD4RT+7XWQP",
        ] {
            let e = text.parse::<JoinCode>().unwrap_err();
            assert_eq!(e.code, codes::BAD_CODE, "{text}");
        }
    }

    #[test]
    fn a_join_code_reads_back_and_is_random() {
        let code = JoinCode::generate().unwrap();
        let text = code.to_string();
        assert_eq!(text.len(), 11);
        assert_eq!(&text[5..6], "-");
        assert_eq!(text.parse::<JoinCode>().unwrap(), code);
        assert_eq!(
            text.to_lowercase()
                .replace('-', " ")
                .parse::<JoinCode>()
                .unwrap(),
            code
        );
        assert_eq!(format!("{code:?}"), "JoinCode(..)");
        assert_ne!(JoinCode::generate().unwrap(), code, "50 random bits");
        let read: JoinCode = "kd4rt 7xwqp".parse().unwrap();
        assert_eq!(read.to_string(), "KD4RT-7XWQP");
        let read: JoinCode = "OIL00-11111".parse().unwrap();
        assert_eq!(read.to_string(), "01100-11111");
    }

    #[test]
    fn the_pre_shared_key_depends_on_the_server_and_the_code() {
        let (a, b) = (
            ServerKey::from_bytes([1; 32]),
            ServerKey::from_bytes([2; 32]),
        );
        let c1: JoinCode = "KD4RT-7XWQP".parse().unwrap();
        let c2: JoinCode = "KD4RT-7XWQQ".parse().unwrap();
        let k = |s: &ServerKey, c: Option<&JoinCode>| *psk(&s.public_key(), c);
        assert_eq!(
            k(&a, Some(&c1)),
            k(&a, Some(&"kd4rt7xwqp".parse().unwrap()))
        );
        assert_ne!(k(&a, Some(&c1)), k(&a, Some(&c2)));
        assert_ne!(k(&a, Some(&c1)), k(&b, Some(&c1)));
        assert_ne!(k(&a, None), k(&a, Some(&c1)));
        assert_ne!(k(&a, None), k(&b, None));
        // Pinned, from Python's `hashlib` apart from this crate: the empty
        // code's key is BLAKE2s over the label and the public key alone.
        let key = ServerKey::from_bytes([7; 32]);
        assert_eq!(
            hex(&k(&key, None)),
            "8820692869d078bd8f75b409c5b95413af5bb47c0162d8ac8d894d57fa0a54de"
        );
        assert_eq!(
            hex(&k(&key, Some(&c1))),
            "3e4eb627a8b496d9672e8fc5b6c32953370381b811f8ea687b3caf3f337151ca"
        );
    }

    #[test]
    fn a_key_file_round_trips_and_shows_its_fingerprint() {
        let key = ServerKey::generate().unwrap();
        let text = key.to_file();
        assert!(text.starts_with("# Real Engine server key."));
        assert!(text.contains(&format!("fingerprint {}\n", key.fingerprint())));
        assert_eq!(ServerKey::from_file(&text).unwrap(), key);
        // The secret itself survives, not only the public half.
        let again = ServerKey::from_file(&text).unwrap();
        assert_eq!(again.secret().to_bytes(), key.secret().to_bytes());
    }

    #[test]
    fn a_damaged_key_file_says_why() {
        let key = ServerKey::from_bytes([3; 32]);
        let text = key.to_file().to_string();
        let other = ServerKey::from_bytes([4; 32]).fingerprint();
        let cases = [
            (String::new(), "no private-key line"),
            (
                text.replace("private-key ", "private-key 00"),
                "not 64 hex digits",
            ),
            (
                text.replace("private-key 0", "private-key g"),
                "not 64 hex digits",
            ),
            (
                text.replace(&key.fingerprint().to_string(), &other.to_string()),
                "but its key's fingerprint is",
            ),
            (
                text.replace("fingerprint ", "fingerprint X"),
                "does not parse",
            ),
            (
                format!("{text}private-key {}\n", "00".repeat(32)),
                "a second `private-key` line (line 5)",
            ),
            (format!("{text}colour blue\n"), "its line 5 is not part of"),
            // A damaged line may hold the secret, so no message quotes one.
            (
                text.replace("private-key ", "private-key="),
                "its line 4 is not part of",
            ),
        ];
        let secret = "03".repeat(32);
        for (text, why) in cases {
            let e = ServerKey::from_file(&text).unwrap_err();
            assert!(e.contains(why), "{e}");
            assert!(!e.contains(&secret[..8]), "{e}");
        }
        // Without its fingerprint line it still reads.
        let bare: String = text
            .lines()
            .filter(|l| !l.starts_with("fingerprint"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(ServerKey::from_file(&bare).unwrap(), key);
    }

    #[test]
    fn a_server_makes_its_key_once_and_keeps_it() {
        let data = WritableDir::memory();
        let (made, new) = ServerKey::load_or_make(&data).unwrap();
        assert!(new);
        let (loaded, new) = ServerKey::load_or_make(&data).unwrap();
        assert!(!new);
        assert_eq!(loaded, made);
        data.write(KEY_FILE, b"private-key nope").unwrap();
        let e = ServerKey::load_or_make(&data).unwrap_err();
        assert_eq!(e.code, codes::SECRETS);
        assert!(e.message.contains("does not parse"), "{}", e.message);
    }

    #[cfg(unix)]
    #[test]
    fn the_key_file_is_its_owners_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("re_net-key-{}", std::process::id()));
        let data = WritableDir::dir(&dir);
        let (key, new) = ServerKey::load_or_make(&data).unwrap();
        assert!(new);
        let path = dir.join(KEY_FILE);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
        assert_eq!(ServerKey::load_or_make(&data).unwrap().0, key);
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "no temporary file stays");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn servers_that_start_at_once_share_one_key() {
        let dir = std::env::temp_dir().join(format!("re_net-keys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data = WritableDir::dir(&dir);
        let keys: Vec<(ServerKey, bool)> = std::thread::scope(|s| {
            let started: Vec<_> = (0..8)
                .map(|_| s.spawn(|| ServerKey::load_or_make(&data).unwrap()))
                .collect();
            started.into_iter().map(|t| t.join().unwrap()).collect()
        });
        assert_eq!(
            keys.iter().filter(|(_, new)| *new).count(),
            1,
            "one made it"
        );
        assert!(keys.iter().all(|(k, _)| *k == keys[0].0));
        assert_eq!(
            ServerKey::load_or_make(&data).unwrap(),
            (keys[0].0.clone(), false)
        );
        let files = std::fs::read_dir(dir.join("net")).unwrap().count();
        assert_eq!(files, 1, "no temporary file stays");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
