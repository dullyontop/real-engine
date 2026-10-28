//! `Noise_NKpsk0_25519_ChaChaPoly_BLAKE2s`, the one Noise pattern `re_net`
//! speaks (ADR-0067 §1, §2), written from the Noise specification, revision
//! 34: the symmetric state (§5.2), the handshake of pattern NK with the
//! `psk0` modifier (§7.5, §9), Split, and sealing with an explicit nonce, for
//! a transport that loses and reorders packets (§11.4).
//!
//! ```text
//! NKpsk0:
//!   <- s
//!   ...
//!   -> psk, e, es
//!   <- e, ee
//! ```
//!
//! Every secret here is wiped when it is dropped: the chaining key, the
//! handshake hash and the cipher key of a handshake in progress, HKDF's
//! outputs, the Diffie-Hellman results and ephemeral keys (x25519-dalek wipes
//! its own) and the transport ciphers (chacha20poly1305 wipes its own). None
//! of these types has a `Debug`.

use blake2::{Blake2s256, Digest};
use chacha20poly1305::aead::{AeadInOut, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce, Tag};
use x25519_dalek::{PublicKey, SharedSecret, StaticSecret};
use zeroize::{Zeroize, Zeroizing};

/// The protocol name, which starts the handshake hash (§8).
const PROTOCOL_NAME: &[u8] = b"Noise_NKpsk0_25519_ChaChaPoly_BLAKE2s";
/// BLAKE2s's output length (HASHLEN) and block length (BLOCKLEN) (§12.8).
const HASHLEN: usize = 32;
const BLOCKLEN: usize = 64;
/// An X25519 public key (DHLEN, §12.1).
pub(crate) const DHLEN: usize = 32;
/// ChaCha20-Poly1305's tag (§12.3).
pub(crate) const TAG: usize = 16;
/// A handshake message with an empty payload, either way: the sender's
/// ephemeral key and the tag of its payload.
pub(crate) const MESSAGE: usize = DHLEN + TAG;

type Hash = [u8; HASHLEN];

/// A handshake message or a packet that does not verify. It carries nothing
/// more, so a failure tells no one why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Error;

/// HASH() over the concatenation of `parts`.
fn hash(parts: &[&[u8]]) -> Hash {
    let mut h = Blake2s256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// HMAC-HASH(key, data) (RFC 2104 with BLAKE2s, §4.3), `data` the
/// concatenation of `parts`. Noise's keys are HASHLEN bytes, shorter than a
/// block, so the key is used as it is. A server's handshake cookies use it
/// too.
pub(crate) fn hmac(key: &Hash, parts: &[&[u8]]) -> Zeroizing<Hash> {
    let mut pad = Zeroizing::new([0x36u8; BLOCKLEN]);
    for (p, k) in pad.iter_mut().zip(key) {
        *p ^= k;
    }
    let mut inner = Blake2s256::new();
    inner.update(&pad[..]);
    for p in parts {
        inner.update(p);
    }
    // Each hash is written straight into memory that is wiped, so no copy
    // of it is left behind.
    let mut inner_hash = Zeroizing::new([0u8; HASHLEN]);
    inner.finalize_into((&mut *inner_hash).into());
    // The outer pad is the key XOR 0x5c, from the inner one.
    for p in pad.iter_mut() {
        *p ^= 0x36 ^ 0x5c;
    }
    let mut outer = Blake2s256::new();
    outer.update(&pad[..]);
    outer.update(&inner_hash[..]);
    let mut out = Zeroizing::new([0u8; HASHLEN]);
    outer.finalize_into((&mut *out).into());
    out
}

/// HKDF(chaining_key, input_key_material, 2) (§4.3).
fn hkdf2(ck: &Hash, ikm: &[u8]) -> (Zeroizing<Hash>, Zeroizing<Hash>) {
    let temp = hmac(ck, &[ikm]);
    let a = hmac(&temp, &[&[1]]);
    let b = hmac(&temp, &[&a[..], &[2]]);
    (a, b)
}

/// HKDF(chaining_key, input_key_material, 3) (§4.3).
fn hkdf3(ck: &Hash, ikm: &[u8]) -> (Zeroizing<Hash>, Zeroizing<Hash>, Zeroizing<Hash>) {
    let temp = hmac(ck, &[ikm]);
    let a = hmac(&temp, &[&[1]]);
    let b = hmac(&temp, &[&a[..], &[2]]);
    let c = hmac(&temp, &[&b[..], &[3]]);
    (a, b, c)
}

/// ChaChaPoly's nonce for number `n`: 32 zero bits, then `n` little-endian
/// (§12.3).
fn nonce(n: u64) -> Nonce {
    let mut b = [0u8; 12];
    b[4..].copy_from_slice(&n.to_le_bytes());
    Nonce::from(b)
}

/// DH(key_pair, public_key) (§12.1): X25519, refusing a result of all zeros,
/// which a public key of low order gives (ADR-0067 §2).
fn dh(secret: &StaticSecret, public: &[u8; DHLEN]) -> Result<SharedSecret, Error> {
    let shared = secret.diffie_hellman(&PublicKey::from(*public));
    if shared.was_contributory() {
        Ok(shared)
    } else {
        Err(Error)
    }
}

/// One direction's transport cipher: §5.1's CipherState, with the nonce given
/// for each packet (§11.4), so packets may arrive in any order.
pub(crate) struct Cipher(ChaCha20Poly1305);

impl Cipher {
    pub(crate) fn new(key: &Hash) -> Cipher {
        Cipher(ChaCha20Poly1305::new(key.into()))
    }

    /// Encrypts `packet[ad..]` in place under number `n`, with `packet[..ad]`
    /// as the associated data, and appends the tag.
    ///
    /// # Errors
    ///
    /// `n` is 2^64 − 1, which Noise reserves (§5.1).
    pub(crate) fn seal(&self, n: u64, packet: &mut Vec<u8>, ad: usize) -> Result<(), Error> {
        if n == u64::MAX {
            return Err(Error);
        }
        let (head, body) = packet.split_at_mut(ad);
        let tag = self
            .0
            .encrypt_inout_detached(&nonce(n), head, body.into())
            .map_err(|_| Error)?;
        packet.extend_from_slice(&tag);
        Ok(())
    }

    /// Opens a packet [`Cipher::seal`] made with number `n` and `ad` bytes of
    /// associated data: checks its tag, then decrypts `packet[ad..]` up to the
    /// tag in place. Returns the plaintext's length. A packet that fails is
    /// left as it was.
    ///
    /// # Errors
    ///
    /// The packet is shorter than `ad` and a tag, `n` is 2^64 − 1, or the tag
    /// does not match: the packet was changed, or sealed under another key,
    /// number or associated data.
    pub(crate) fn open(&self, n: u64, packet: &mut [u8], ad: usize) -> Result<usize, Error> {
        if n == u64::MAX {
            return Err(Error);
        }
        let end = packet
            .len()
            .checked_sub(TAG)
            .filter(|&end| end >= ad)
            .ok_or(Error)?;
        let (sealed, tag) = packet.split_at_mut(end);
        let tag = Tag::try_from(&tag[..]).map_err(|_| Error)?;
        let (head, body) = sealed.split_at_mut(ad);
        self.0
            .decrypt_inout_detached(&nonce(n), head, body.into(), &tag)
            .map_err(|_| Error)?;
        Ok(end - ad)
    }
}

/// A finished handshake: a cipher for each direction (Split, §5.2).
pub(crate) struct Session {
    /// This side's.
    pub(crate) send: Cipher,
    /// The other side's.
    pub(crate) recv: Cipher,
}

/// The symmetric state of a handshake (§5.2): the chaining key `ck`, the
/// handshake hash `h`, and the CipherState's key `k` and counter `n` once a
/// key is mixed in.
#[derive(Clone)]
struct Symmetric {
    ck: Hash,
    h: Hash,
    k: Option<Hash>,
    n: u64,
}

impl Drop for Symmetric {
    fn drop(&mut self) {
        self.ck.zeroize();
        self.h.zeroize();
        self.k.zeroize();
    }
}

impl Symmetric {
    /// InitializeSymmetric(protocol_name) and MixHash(prologue) (§5.2, §5.3).
    /// The name is longer than HASHLEN, so `h` starts as its hash.
    fn new(prologue: &[u8]) -> Symmetric {
        let h = hash(&[PROTOCOL_NAME]);
        let mut s = Symmetric {
            ck: h,
            h,
            k: None,
            n: 0,
        };
        s.mix_hash(prologue);
        s
    }

    fn mix_hash(&mut self, data: &[u8]) {
        self.h = hash(&[&self.h, data]);
    }

    fn mix_key(&mut self, ikm: &[u8]) {
        let (ck, k) = hkdf2(&self.ck, ikm);
        self.ck = *ck;
        self.k = Some(*k);
        self.n = 0;
    }

    fn mix_key_and_hash(&mut self, ikm: &[u8]) {
        let (ck, temp_h, k) = hkdf3(&self.ck, ikm);
        self.ck = *ck;
        self.mix_hash(&temp_h[..]);
        self.k = Some(*k);
        self.n = 0;
    }

    /// EncryptAndHash(plaintext), appended to `out`. NKpsk0 mixes in a key
    /// with its first token, so there always is one here. A handshake
    /// encrypts once each way, so `n` never comes near its limit.
    fn encrypt_and_hash(&mut self, plaintext: &[u8], out: &mut Vec<u8>) -> Result<(), Error> {
        let k = self.k.as_ref().ok_or(Error)?;
        let start = out.len();
        out.extend_from_slice(plaintext);
        let tag = ChaCha20Poly1305::new(k.into())
            .encrypt_inout_detached(&nonce(self.n), &self.h, (&mut out[start..]).into())
            .map_err(|_| Error)?;
        out.extend_from_slice(&tag);
        self.n += 1;
        self.mix_hash(&out[start..]);
        Ok(())
    }

    /// DecryptAndHash(ciphertext): the plaintext.
    fn decrypt_and_hash(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, Error> {
        let k = self.k.as_ref().ok_or(Error)?;
        let end = ciphertext.len().checked_sub(TAG).ok_or(Error)?;
        let tag = Tag::try_from(&ciphertext[end..]).map_err(|_| Error)?;
        let mut plaintext = ciphertext[..end].to_vec();
        ChaCha20Poly1305::new(k.into())
            .decrypt_inout_detached(
                &nonce(self.n),
                &self.h,
                plaintext.as_mut_slice().into(),
                &tag,
            )
            .map_err(|_| Error)?;
        self.n += 1;
        self.mix_hash(ciphertext);
        Ok(plaintext)
    }

    /// Split() (§5.2): the initiator's sending cipher, then the responder's.
    fn split(&self) -> (Cipher, Cipher) {
        let (k1, k2) = hkdf2(&self.ck, &[]);
        (Cipher::new(&k1), Cipher::new(&k2))
    }
}

/// The first steps both sides take: `<- s` as the pre-message, then the
/// `psk` token.
fn begin(prologue: &[u8], server: &[u8; DHLEN], psk: &[u8; 32]) -> Symmetric {
    let mut sym = Symmetric::new(prologue);
    sym.mix_hash(server);
    sym.mix_key_and_hash(psk);
    sym
}

/// A client between its first handshake message and the server's answer.
pub(crate) struct Initiator {
    sym: Symmetric,
    e: StaticSecret,
}

impl Initiator {
    /// Writes the first message, `-> psk, e, es`, to the server whose static
    /// key is `rs`, with the pre-shared key `psk`, the `prologue`, the
    /// ephemeral key `e` and `payload`.
    ///
    /// # Errors
    ///
    /// `rs` is a key of low order.
    pub(crate) fn start(
        rs: &[u8; DHLEN],
        psk: &[u8; 32],
        prologue: &[u8],
        e: &StaticSecret,
        payload: &[u8],
    ) -> Result<(Initiator, Vec<u8>), Error> {
        let mut sym = begin(prologue, rs, psk);
        let e_pub = PublicKey::from(e);
        let mut msg = Vec::with_capacity(MESSAGE + payload.len());
        msg.extend_from_slice(e_pub.as_bytes());
        sym.mix_hash(e_pub.as_bytes());
        // With a pre-shared key, `e` is mixed into the key as well (§9.2).
        sym.mix_key(e_pub.as_bytes());
        sym.mix_key(dh(e, rs)?.as_bytes());
        sym.encrypt_and_hash(payload, &mut msg)?;
        Ok((Initiator { sym, e: e.clone() }, msg))
    }

    /// Reads the server's answer, `<- e, ee`: returns its payload, the
    /// session (this client's sending cipher is the initiator's) and the
    /// handshake hash. The handshake itself does not change, so an answer
    /// that fails (a forgery, or one changed on the way) leaves it ready for
    /// the real one.
    ///
    /// # Errors
    ///
    /// The answer is short, its key is of low order, or its tag does not
    /// match.
    pub(crate) fn finish(&self, msg: &[u8]) -> Result<(Vec<u8>, Session, Hash), Error> {
        let re: [u8; DHLEN] = msg
            .get(..DHLEN)
            .and_then(|b| b.try_into().ok())
            .ok_or(Error)?;
        let mut sym = self.sym.clone();
        sym.mix_hash(&re);
        sym.mix_key(&re);
        sym.mix_key(dh(&self.e, &re)?.as_bytes());
        let payload = sym.decrypt_and_hash(&msg[DHLEN..])?;
        let (send, recv) = sym.split();
        Ok((payload, Session { send, recv }, sym.h))
    }
}

/// A server's whole part of a handshake: reads the first message,
/// `-> psk, e, es`, with its static key `s` (whose public half is `s_pub`),
/// the pre-shared key and the prologue, and answers `<- e, ee` with the
/// ephemeral key `e` and `payload`. Returns the client's payload, the answer,
/// the session (this server's sending cipher is the responder's) and the
/// handshake hash.
///
/// # Errors
///
/// The message is short, its key is of low order, or its tag does not match:
/// the client used another pre-shared key (join code), server key or
/// prologue, or the message was changed on the way.
pub(crate) fn respond(
    s: &StaticSecret,
    s_pub: &[u8; DHLEN],
    psk: &[u8; 32],
    prologue: &[u8],
    msg: &[u8],
    e: &StaticSecret,
    payload: &[u8],
) -> Result<(Vec<u8>, Vec<u8>, Session, Hash), Error> {
    let mut sym = begin(prologue, s_pub, psk);
    let re: [u8; DHLEN] = msg
        .get(..DHLEN)
        .and_then(|b| b.try_into().ok())
        .ok_or(Error)?;
    sym.mix_hash(&re);
    sym.mix_key(&re);
    sym.mix_key(dh(s, &re)?.as_bytes());
    let got = sym.decrypt_and_hash(&msg[DHLEN..])?;
    let e_pub = PublicKey::from(e);
    let mut answer = Vec::with_capacity(MESSAGE + payload.len());
    answer.extend_from_slice(e_pub.as_bytes());
    sym.mix_hash(e_pub.as_bytes());
    sym.mix_key(e_pub.as_bytes());
    sym.mix_key(dh(e, &re)?.as_bytes());
    sym.encrypt_and_hash(payload, &mut answer)?;
    let (initiators, responders) = sym.split();
    Ok((
        got,
        answer,
        Session {
            send: responders,
            recv: initiators,
        },
        sym.h,
    ))
}

/// Two ends of a session from a handshake with fixed keys, for tests: the
/// client's, then the server's.
#[cfg(test)]
pub(crate) fn test_pair() -> (Session, Session) {
    let s = StaticSecret::from([7; 32]);
    let s_pub = PublicKey::from(&s).to_bytes();
    let (client, msg1) =
        Initiator::start(&s_pub, &[9; 32], b"test", &StaticSecret::from([1; 32]), &[]).unwrap();
    let (_, msg2, server, _) = respond(
        &s,
        &s_pub,
        &[9; 32],
        b"test",
        &msg1,
        &StaticSecret::from([2; 32]),
        &[],
    )
    .unwrap();
    let (_, client, _) = client.finish(&msg2).unwrap();
    (client, server)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        assert!(s.len().is_multiple_of(2), "{s}");
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn key(s: &str) -> [u8; 32] {
        hex(s).try_into().unwrap()
    }

    fn field<'a>(v: &'a serde_json::Value, name: &str) -> &'a str {
        v[name].as_str().unwrap_or_else(|| panic!("no {name}"))
    }

    /// Plays one vector in cacophony's layout: both handshake messages and
    /// the handshake hash, then each transport message, sealed and opened at
    /// the `nonce` it names (or at each side's next one, from 0), all byte for
    /// byte.
    fn play(v: &serde_json::Value) {
        assert_eq!(field(v, "protocol_name").as_bytes(), PROTOCOL_NAME);
        let prologue = hex(field(v, "init_prologue"));
        assert_eq!(prologue, hex(field(v, "resp_prologue")));
        let psk = key(v["init_psks"][0].as_str().unwrap());
        let rs = key(field(v, "init_remote_static"));
        let s = StaticSecret::from(key(field(v, "resp_static")));
        assert_eq!(PublicKey::from(&s).as_bytes(), &rs);
        let ie = StaticSecret::from(key(field(v, "init_ephemeral")));
        let re = StaticSecret::from(key(field(v, "resp_ephemeral")));
        let msgs = v["messages"].as_array().unwrap();
        let payload = |i: usize| hex(field(&msgs[i], "payload"));
        let wire = |i: usize| hex(field(&msgs[i], "ciphertext"));

        let (init, m1) = Initiator::start(&rs, &psk, &prologue, &ie, &payload(0)).unwrap();
        assert_eq!(m1, wire(0), "message 1");
        let (got, m2, server, server_hash) =
            respond(&s, &rs, &psk, &prologue, &m1, &re, &payload(1)).unwrap();
        assert_eq!(got, payload(0));
        assert_eq!(m2, wire(1), "message 2");
        let (got, client, client_hash) = init.finish(&m2).unwrap();
        assert_eq!(got, payload(1));
        assert_eq!(client_hash.to_vec(), hex(field(v, "handshake_hash")));
        assert_eq!(server_hash, client_hash);
        let (mut next_i, mut next_r) = (0u64, 0u64);
        for (i, m) in msgs.iter().enumerate().skip(2) {
            // Without `from`, the messages alternate, the initiator first.
            let from_initiator = match m["from"].as_str() {
                Some(from) => from == "initiator",
                None => i % 2 == 0,
            };
            let (send, recv, next) = if from_initiator {
                (&client.send, &server.recv, &mut next_i)
            } else {
                (&server.send, &client.recv, &mut next_r)
            };
            let n = m["nonce"].as_u64().unwrap_or(*next);
            *next = n + 1;
            let mut packet = payload(i);
            send.seal(n, &mut packet, 0).unwrap();
            assert_eq!(packet, wire(i), "transport message {i} at nonce {n}");
            let len = recv.open(n, &mut packet, 0).unwrap();
            assert_eq!(packet[..len], payload(i));
        }
    }

    /// cacophony's vector for this pattern, as snow 0.10.0 ships it in
    /// `tests/vectors/cacophony.txt`.
    #[test]
    fn matches_the_cacophony_vector() {
        let vs: serde_json::Value =
            serde_json::from_str(include_str!("../tests/data/nkpsk0-cacophony.json")).unwrap();
        assert_eq!(vs.as_array().unwrap().len(), 1);
        play(&vs[0]);
    }

    /// Vectors snow 0.10.0 made for these parameters with transport version
    /// 2's prologue layout, empty and non-empty payloads, and transport
    /// messages at explicit nonces out of order: an independent
    /// implementation's bytes, with no snow dependency.
    #[test]
    fn matches_the_vectors_snow_made() {
        let vs: serde_json::Value =
            serde_json::from_str(include_str!("../tests/data/nkpsk0-snow.json")).unwrap();
        assert_eq!(vs.as_array().unwrap().len(), 4);
        for v in vs.as_array().unwrap() {
            play(v);
        }
    }

    struct Keys {
        s: StaticSecret,
        s_pub: [u8; 32],
        psk: [u8; 32],
        prologue: &'static [u8],
    }

    fn keys() -> Keys {
        let s = StaticSecret::from([7; 32]);
        Keys {
            s_pub: PublicKey::from(&s).to_bytes(),
            s,
            psk: [9; 32],
            prologue: b"real-engine/net/2 and the rest",
        }
    }

    fn eph(b: u8) -> StaticSecret {
        StaticSecret::from([b; 32])
    }

    fn first(k: &Keys) -> (Initiator, Vec<u8>) {
        Initiator::start(&k.s_pub, &k.psk, k.prologue, &eph(1), &[]).unwrap()
    }

    fn answer(k: &Keys, msg1: &[u8]) -> Result<Vec<u8>, Error> {
        respond(&k.s, &k.s_pub, &k.psk, k.prologue, msg1, &eph(2), &[]).map(|(_, m, _, _)| m)
    }

    #[test]
    fn empty_payloads_make_messages_of_48_bytes() {
        let k = keys();
        let (init, msg1) = first(&k);
        assert_eq!(msg1.len(), MESSAGE);
        let msg2 = answer(&k, &msg1).unwrap();
        assert_eq!(msg2.len(), MESSAGE);
        let (payload, ..) = init.finish(&msg2).unwrap();
        assert!(payload.is_empty());
    }

    #[test]
    fn a_flipped_bit_anywhere_in_the_first_message_fails_it() {
        let k = keys();
        let (_, msg1) = first(&k);
        for bit in 0..msg1.len() * 8 {
            let mut m = msg1.clone();
            m[bit / 8] ^= 1 << (bit % 8);
            assert_eq!(answer(&k, &m), Err(Error), "bit {bit}");
        }
        assert!(answer(&k, &msg1).is_ok());
    }

    #[test]
    fn a_flipped_bit_anywhere_in_the_answer_fails_it_and_changes_nothing() {
        let k = keys();
        let (init, msg1) = first(&k);
        let msg2 = answer(&k, &msg1).unwrap();
        for bit in 0..msg2.len() * 8 {
            let mut m = msg2.clone();
            m[bit / 8] ^= 1 << (bit % 8);
            assert!(init.finish(&m).is_err(), "bit {bit}");
        }
        // The real answer still finishes, with the server's keys.
        let (_, client, _) = init.finish(&msg2).unwrap();
        let (_, _, server, _) =
            respond(&k.s, &k.s_pub, &k.psk, k.prologue, &msg1, &eph(2), &[]).unwrap();
        let mut p = b"after the forgeries".to_vec();
        client.send.seal(0, &mut p, 0).unwrap();
        assert!(server.recv.open(0, &mut p, 0).is_ok());
    }

    #[test]
    fn short_or_long_messages_fail() {
        let k = keys();
        let (init, msg1) = first(&k);
        let msg2 = answer(&k, &msg1).unwrap();
        for n in 0..MESSAGE {
            assert!(answer(&k, &msg1[..n]).is_err(), "msg1 of {n} bytes");
            assert!(init.finish(&msg2[..n]).is_err(), "msg2 of {n} bytes");
        }
        let mut longer = msg1.clone();
        longer.push(0);
        assert!(answer(&k, &longer).is_err());
    }

    #[test]
    fn another_join_code_server_key_or_prologue_fails_the_servers_first_read() {
        let k = keys();
        let (_, msg1) = first(&k);
        let wrong_psk = respond(&k.s, &k.s_pub, &[8; 32], k.prologue, &msg1, &eph(2), &[]);
        assert!(wrong_psk.is_err());
        let wrong_prologue = respond(&k.s, &k.s_pub, &k.psk, b"other", &msg1, &eph(2), &[]);
        assert!(wrong_prologue.is_err());
        // A client that encrypted to another key (an impostor's Challenge).
        let other = PublicKey::from(&StaticSecret::from([6; 32])).to_bytes();
        let (_, msg1) = Initiator::start(&other, &k.psk, k.prologue, &eph(1), &[]).unwrap();
        assert!(answer(&k, &msg1).is_err());
    }

    /// The public keys of small order on Curve25519 (in their canonical
    /// encodings and some others), each of which gives an all-zero
    /// Diffie-Hellman result.
    const LOW_ORDER: [[u8; 32]; 7] = [
        [0; 32],
        {
            let mut k = [0; 32];
            k[0] = 1;
            k
        },
        [
            0xe0, 0xeb, 0x7a, 0x7c, 0x3b, 0x41, 0xb8, 0xae, 0x16, 0x56, 0xe3, 0xfa, 0xf1, 0x9f,
            0xc4, 0x6a, 0xda, 0x09, 0x8d, 0xeb, 0x9c, 0x32, 0xb1, 0xfd, 0x86, 0x62, 0x05, 0x16,
            0x5f, 0x49, 0xb8, 0x00,
        ],
        [
            0x5f, 0x9c, 0x95, 0xbc, 0xa3, 0x50, 0x8c, 0x24, 0xb1, 0xd0, 0xb1, 0x55, 0x9c, 0x83,
            0xef, 0x5b, 0x04, 0x44, 0x5c, 0xc4, 0x58, 0x1c, 0x8e, 0x86, 0xd8, 0x22, 0x4e, 0xdd,
            0xd0, 0x9f, 0x11, 0x57,
        ],
        {
            // p − 1
            let mut k = [0xff; 32];
            k[0] = 0xec;
            k[31] = 0x7f;
            k
        },
        {
            // p
            let mut k = [0xff; 32];
            k[0] = 0xed;
            k[31] = 0x7f;
            k
        },
        {
            // p + 1
            let mut k = [0xff; 32];
            k[0] = 0xee;
            k[31] = 0x7f;
            k
        },
    ];

    #[test]
    fn a_low_order_key_is_refused_on_either_side() {
        let k = keys();
        let (init, msg1) = first(&k);
        let msg2 = answer(&k, &msg1).unwrap();
        for point in LOW_ORDER {
            // The client's ephemeral key in its first message.
            let mut m = point.to_vec();
            m.extend_from_slice(&msg1[DHLEN..]);
            assert_eq!(answer(&k, &m), Err(Error), "{point:02x?}");
            // The server's ephemeral key in its answer.
            let mut m = point.to_vec();
            m.extend_from_slice(&msg2[DHLEN..]);
            assert!(init.finish(&m).is_err(), "{point:02x?}");
            // A server key of low order in a Challenge.
            let started = Initiator::start(&point, &k.psk, k.prologue, &eph(1), &[]);
            assert!(started.is_err(), "{point:02x?}");
        }
    }

    #[test]
    fn a_packet_opens_only_with_its_key_number_and_associated_data() {
        let (client, server) = test_pair();
        let mut packet = b"head|the body".to_vec();
        client.send.seal(41, &mut packet, 5).unwrap();
        assert_eq!(packet.len(), 13 + TAG);
        assert_ne!(&packet[5..13], b"the body", "the body is encrypted");
        assert_eq!(&packet[..5], b"head|", "the associated data is not");
        for bit in 0..packet.len() * 8 {
            let mut p = packet.clone();
            p[bit / 8] ^= 1 << (bit % 8);
            assert!(server.recv.open(41, &mut p, 5).is_err(), "bit {bit}");
        }
        let mut p = packet.clone();
        assert!(server.recv.open(42, &mut p, 5).is_err(), "another number");
        assert_eq!(p, packet, "a packet that fails is left as it was");
        assert!(
            server.recv.open(41, &mut p, 4).is_err(),
            "less associated data"
        );
        assert!(
            server.send.open(41, &mut p, 5).is_err(),
            "the other direction"
        );
        assert!(client.recv.open(41, &mut p, 5).is_err(), "the client's own");
        assert!(server.recv.open(41, &mut p[..20], 5).is_err(), "truncated");
        assert!(
            server.recv.open(41, &mut p[..4], 5).is_err(),
            "shorter than its head"
        );
        let n = server.recv.open(41, &mut p, 5).unwrap();
        assert_eq!(&p[5..5 + n], b"the body");
    }

    #[test]
    fn the_reserved_number_never_seals_or_opens() {
        let (client, server) = test_pair();
        let mut p = b"x".to_vec();
        assert!(client.send.seal(u64::MAX, &mut p, 0).is_err());
        assert_eq!(p, b"x");
        client.send.seal(u64::MAX - 1, &mut p, 0).unwrap();
        assert!(server.recv.open(u64::MAX, &mut p.clone(), 0).is_err());
        assert!(server.recv.open(u64::MAX - 1, &mut p, 0).is_ok());
    }

    #[test]
    fn an_empty_body_still_carries_a_tag() {
        let (client, server) = test_pair();
        let mut p = b"header".to_vec();
        client.send.seal(0, &mut p, 6).unwrap();
        assert_eq!(p.len(), 6 + TAG);
        assert_eq!(server.recv.open(0, &mut p, 6), Ok(0));
    }

    #[test]
    fn every_secret_wipes_itself_when_dropped() {
        // Checked when this test builds: each type that holds a secret here
        // wipes it on drop, so a build without the `zeroize` features (or a
        // version that drops them) fails. A BLAKE2s hasher is its state and
        // its block buffer, which each wipe themselves; `Symmetric` wipes its
        // own fields.
        fn wiped<T: zeroize::ZeroizeOnDrop>() {}
        wiped::<StaticSecret>();
        wiped::<SharedSecret>();
        wiped::<ChaCha20Poly1305>();
        wiped::<blake2::Blake2sVarCore>();
        wiped::<blake2::digest::block_api::Buffer<blake2::Blake2sVarCore>>();
        wiped::<Zeroizing<Hash>>();
        wiped::<Zeroizing<Vec<u8>>>();
    }
}
