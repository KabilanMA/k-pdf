//! Password-protected PDFs: the standard security handler (ISO 32000-2
//! §7.6.4), revisions 2 to 6 — RC4 with 40- to 128-bit keys, AES-128 and
//! AES-256 — for reading, and for saving with the same protection.
//!
//! A file's key is worked out once when it is opened, from the password
//! (the user's, or the owner's); each object's strings and streams are
//! then decrypted as the object is read, so nothing more is kept in memory
//! than for an unprotected file. Saving encrypts the objects again with
//! the same key and writes the same encryption dictionary and file ID,
//! so the file keeps its passwords and permissions.

mod aes;
mod hash;

use crate::error::{Error, Result};
use crate::object::{Dictionary, Object, Stream};
use aes::Aes;
use hash::{md5, sha256, sha512};

/// How strings or streams are encrypted (a crypt filter's method).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Method {
    Identity,
    Rc4,
    Aes128,
    Aes256,
}

impl Method {
    fn strength(self) -> u8 {
        match self {
            Method::Identity => 0,
            Method::Rc4 => 1,
            Method::Aes128 => 2,
            Method::Aes256 => 3,
        }
    }

    fn max_by_strength(self, other: Method) -> Method {
        if other.strength() > self.strength() { other } else { self }
    }
}

/// The password padding of revisions 2–4 (Algorithm 2, step a).
const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08, 0x2E, 0x2E, 0x00, 0xB6, 0xD0,
    0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// An unlocked file: its key and how its strings and streams are
/// encrypted.
#[derive(Clone, Debug)]
pub(crate) struct Crypt {
    key: Vec<u8>,
    strings: Method,
    streams: Method,
    /// Revision 4+ with `/EncryptMetadata false`: the XMP metadata stream
    /// is not encrypted.
    encrypt_metadata: bool,
    /// The encryption dictionary, as in the file (written again on save).
    pub dict: Dictionary,
}

impl Crypt {
    /// Unlocks a file with encryption dictionary `dict` (its entries
    /// resolved) and first file ID `id`, with `password` (the user's or
    /// the owner's; an empty one is tried as is). Fails with
    /// [`Error::PasswordNeeded`] or [`Error::WrongPassword`], or
    /// [`Error::Encrypted`] for encryption other than with a password.
    pub fn unlock(dict: &Dictionary, id: &[u8], password: &str) -> Result<Crypt> {
        let name = |k: &[u8]| dict.get(k).and_then(Object::as_name).map(<[u8]>::to_vec);
        let int = |k: &[u8]| dict.get(k).and_then(Object::as_i64);
        let bytes = |k: &[u8]| dict.get(k).and_then(Object::as_string).map(<[u8]>::to_vec).unwrap_or_default();
        if name(b"Filter").as_deref() != Some(b"Standard") {
            let filter = name(b"Filter").map(|f| String::from_utf8_lossy(&f).into_owned()).unwrap_or_default();
            return Err(Error::Encrypted(format!("it is encrypted for certain people only (the \"{filter}\" security handler), not with a password")));
        }
        let v = int(b"V").unwrap_or(0);
        let r = int(b"R").unwrap_or(2);
        let p = int(b"P").unwrap_or(-1) as i32;
        let (o, u) = (bytes(b"O"), bytes(b"U"));
        let encrypt_metadata = dict.get(b"EncryptMetadata").and_then(Object::as_bool).unwrap_or(true);

        // The methods, and the key length in bytes (revisions 2–4).
        let bits = |l: i64| if l <= 32 { l * 8 } else { l }; // some write bytes
        let mut length = int(b"Length").map_or(40, bits);
        let (strings, streams) = match v {
            1 => {
                length = 40;
                (Method::Rc4, Method::Rc4)
            }
            2 | 3 => (Method::Rc4, Method::Rc4),
            4 | 5 => {
                let filters = dict.get(b"CF").and_then(Object::as_dict);
                let method = |key: &[u8]| -> Result<(Method, Option<i64>)> {
                    let Some(f) = name(key) else { return Ok((Method::Identity, None)) };
                    if f == b"Identity" {
                        return Ok((Method::Identity, None));
                    }
                    let cf = filters.and_then(|c| c.get(&f)).and_then(Object::as_dict);
                    let cfm = cf.and_then(|c| c.get(b"CFM")).and_then(Object::as_name).unwrap_or(b"None");
                    let len = cf.and_then(|c| c.get(b"Length")).and_then(Object::as_i64).map(bits);
                    Ok((
                        match cfm {
                            b"V2" => Method::Rc4,
                            b"AESV2" => Method::Aes128,
                            b"AESV3" => Method::Aes256,
                            b"None" => Method::Identity,
                            other => return Err(Error::Encrypted(format!("its encryption (\"{}\") isn't supported", String::from_utf8_lossy(other)))),
                        },
                        len,
                    ))
                };
                let (s, sl) = method(b"StrF")?;
                let (m, ml) = method(b"StmF")?;
                length = ml.or(sl).unwrap_or(128);
                (s, m)
            }
            _ => return Err(Error::Encrypted(format!("its encryption (version {v}) isn't supported"))),
        };
        let n = ((length / 8) as usize).clamp(5, 16);

        let (key, _owner) = if r >= 5 {
            unlock_aes256(&o, &u, &bytes(b"OE"), &bytes(b"UE"), r, password)?
        } else {
            unlock_rc4(&o, &u, p, id, r, n, encrypt_metadata, password)?
        };
        Ok(Crypt { key, strings, streams, encrypt_metadata, dict: dict.clone() })
    }

    /// How it is encrypted, for people: "AES-256", "RC4, 40-bit", …
    pub fn describe(&self) -> String {
        match self.streams.max_by_strength(self.strings) {
            Method::Aes256 => "AES-256".into(),
            Method::Aes128 => "AES-128".into(),
            Method::Rc4 => format!("RC4, {}-bit", self.key.len() * 8),
            Method::Identity => "none (only the password is checked)".into(),
        }
    }

    /// The key for object `num` `generation` (Algorithm 1; AES-256 uses the
    /// file's key as it is).
    fn object_key(&self, num: u32, generation: u16, method: Method) -> Vec<u8> {
        if method == Method::Aes256 {
            return self.key.clone();
        }
        let salt: &[u8] = if method == Method::Aes128 { b"sAlT" } else { b"" };
        let digest = md5(&[&self.key, &num.to_le_bytes()[..3], &generation.to_le_bytes(), salt]);
        digest[..(self.key.len() + 5).min(16)].to_vec()
    }

    /// Decrypts the strings and stream data of object `num` `generation`
    /// in place.
    pub fn decrypt_object(&self, num: u32, generation: u16, obj: &mut Object) {
        self.apply(num, generation, obj, false);
    }

    /// Encrypts the strings and stream data of an object written as `num`
    /// `generation`, in place.
    pub fn encrypt_object(&self, num: u32, generation: u16, obj: &mut Object) {
        self.apply(num, generation, obj, true);
    }

    fn apply(&self, num: u32, generation: u16, obj: &mut Object, encrypt: bool) {
        let string_key = (self.strings != Method::Identity).then(|| self.object_key(num, generation, self.strings));
        let mut strings = |s: &mut Vec<u8>| {
            if let Some(key) = &string_key {
                *s = crypt(self.strings, key, std::mem::take(s), encrypt);
            }
        };
        match obj {
            Object::Stream(stream) => {
                walk(&mut stream.dict, &mut strings);
                if self.stream_is_encrypted(stream) {
                    let key = self.object_key(num, generation, self.streams);
                    stream.raw = crypt(self.streams, &key, std::mem::take(&mut stream.raw), encrypt);
                }
            }
            Object::Dictionary(d) => walk(d, &mut strings),
            other => walk_object(other, &mut strings),
        }
    }

    /// Whether a stream's data is encrypted: not cross-reference streams,
    /// nor the metadata when the file says so, nor streams with their own
    /// (identity) crypt filter.
    fn stream_is_encrypted(&self, stream: &Stream) -> bool {
        if self.streams == Method::Identity {
            return false;
        }
        match stream.dict.type_name() {
            Some(b"XRef") => return false,
            Some(b"Metadata") if !self.encrypt_metadata => return false,
            _ => {}
        }
        let crypt_filter = match stream.dict.get(b"Filter") {
            Some(Object::Name(n)) => n == b"Crypt",
            Some(Object::Array(a)) => a.first().and_then(Object::as_name) == Some(b"Crypt"),
            _ => false,
        };
        !crypt_filter
    }
}

/// Every string in `d` (but a signature's `/Contents`, which is never
/// encrypted).
fn walk(d: &mut Dictionary, f: &mut impl FnMut(&mut Vec<u8>)) {
    let signature = d.type_name() == Some(b"Sig") || d.contains(b"ByteRange");
    for (k, v) in d.iter_mut() {
        if signature && k == b"Contents" {
            continue;
        }
        walk_object(v, f);
    }
}

fn walk_object(obj: &mut Object, f: &mut impl FnMut(&mut Vec<u8>)) {
    match obj {
        Object::String(s) => f(s),
        Object::Array(items) => items.iter_mut().for_each(|o| walk_object(o, f)),
        Object::Dictionary(d) => walk(d, f),
        Object::Stream(s) => walk(&mut s.dict, f),
        _ => {}
    }
}

/// Encrypts or decrypts `data` with `key`. AES data starts with its
/// 16-byte initialization vector and is padded to whole blocks (PKCS #7).
fn crypt(method: Method, key: &[u8], mut data: Vec<u8>, encrypt: bool) -> Vec<u8> {
    match method {
        Method::Identity => data,
        Method::Rc4 => {
            rc4(key, &mut data);
            data
        }
        Method::Aes128 | Method::Aes256 => {
            let aes = Aes::new(key);
            if encrypt {
                let iv = random_iv();
                let pad = 16 - data.len() % 16;
                data.extend(std::iter::repeat_n(pad as u8, pad));
                aes.cbc_encrypt(&iv, &mut data);
                let mut out = iv.to_vec();
                out.append(&mut data);
                out
            } else {
                if data.len() < 16 {
                    return Vec::new();
                }
                let iv: [u8; 16] = data[..16].try_into().expect("16 bytes");
                data.drain(..16);
                aes.cbc_decrypt(&iv, &mut data);
                // Padding: n bytes of n (left alone when damaged).
                if let Some(&n) = data.last()
                    && (1..=16).contains(&n)
                    && data.len() >= n as usize
                    && data[data.len() - n as usize..].iter().all(|&b| b == n)
                {
                    data.truncate(data.len() - n as usize);
                }
                data
            }
        }
    }
}

/// RC4 in place.
fn rc4(key: &[u8], data: &mut [u8]) {
    if key.is_empty() {
        return;
    }
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let (mut i, mut j) = (0u8, 0u8);
    for b in data {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[i as usize]);
        s.swap(i as usize, j as usize);
        *b ^= s[s[i as usize].wrapping_add(s[j as usize]) as usize];
    }
}

/// An initialization vector no two encryptions share: the time, a
/// counter and the process's random hash seed, hashed. (It needs to be
/// unpredictable, not secret.)
fn random_iv() -> [u8; 16] {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let mut seed = h.finish().to_le_bytes().to_vec();
    seed.extend_from_slice(&time.to_le_bytes());
    sha256(&seed)[..16].try_into().expect("16 bytes")
}

/// A password as revisions 2–4 take it: Latin-1 bytes (other characters
/// can't be typed into such a password), padded or cut to 32 bytes.
fn padded_password(password: &str) -> [u8; 32] {
    let bytes: Vec<u8> = password.chars().map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?')).collect();
    let mut out = PAD;
    let n = bytes.len().min(32);
    out[..n].copy_from_slice(&bytes[..n]);
    out[n..].copy_from_slice(&PAD[..32 - n]);
    out
}

/// Revisions 2–4: the file key from the user password `padded`
/// (Algorithm 2).
#[allow(clippy::too_many_arguments)]
fn rc4_key(padded: &[u8; 32], o: &[u8], p: i32, id: &[u8], r: i64, n: usize, encrypt_metadata: bool) -> Vec<u8> {
    let no_metadata: &[u8] = if r >= 4 && !encrypt_metadata { &[0xff; 4] } else { &[] };
    let o = &o[..o.len().min(32)];
    let mut key = md5(&[padded, o, &p.to_le_bytes(), id, no_metadata]).to_vec();
    if r >= 3 {
        for _ in 0..50 {
            key = md5(&[&key[..n]]).to_vec();
        }
    }
    key.truncate(n);
    key
}

/// Whether `key` is the right one: the user check value it makes matches
/// `/U` (Algorithms 4 and 5).
fn rc4_user_ok(key: &[u8], u: &[u8], id: &[u8], r: i64) -> bool {
    if r == 2 {
        let mut check = PAD;
        rc4(key, &mut check);
        return u.len() >= 32 && u[..32] == check;
    }
    let mut check = md5(&[&PAD, id]);
    rc4(key, &mut check);
    for i in 1..=19u8 {
        let k: Vec<u8> = key.iter().map(|b| b ^ i).collect();
        rc4(&k, &mut check);
    }
    u.len() >= 16 && u[..16] == check
}

/// Revisions 2–4: the file key from the user or the owner password.
#[allow(clippy::too_many_arguments)]
fn unlock_rc4(o: &[u8], u: &[u8], p: i32, id: &[u8], r: i64, n: usize, encrypt_metadata: bool, password: &str) -> Result<(Vec<u8>, bool)> {
    let user = |padded: &[u8; 32]| {
        let key = rc4_key(padded, o, p, id, r, n, encrypt_metadata);
        rc4_user_ok(&key, u, id, r).then_some(key)
    };
    let padded = padded_password(password);
    // The owner password decrypts /O into the user password
    // (Algorithm 7).
    let as_owner = || {
        let mut k = md5(&[&padded]).to_vec();
        if r >= 3 {
            for _ in 0..50 {
                k = md5(&[&k]).to_vec();
            }
        }
        k.truncate(n);
        let mut user_password: [u8; 32] = std::array::from_fn(|i| o.get(i).copied().unwrap_or(0));
        if r == 2 {
            rc4(&k, &mut user_password);
        } else {
            for i in (0..=19u8).rev() {
                let ki: Vec<u8> = k.iter().map(|b| b ^ i).collect();
                rc4(&ki, &mut user_password);
            }
        }
        user(&user_password)
    };
    if let Some(key) = as_owner() {
        return Ok((key, true));
    }
    if let Some(key) = user(&padded) {
        return Ok((key, false));
    }
    Err(if password.is_empty() { Error::PasswordNeeded } else { Error::WrongPassword })
}

/// Revisions 5 and 6 (AES-256): the file key from the user or the owner
/// password (Algorithms 2.A, 2.B).
fn unlock_aes256(o: &[u8], u: &[u8], oe: &[u8], ue: &[u8], r: i64, password: &str) -> Result<(Vec<u8>, bool)> {
    if o.len() < 48 || u.len() < 48 || oe.len() < 32 || ue.len() < 32 {
        return Err(Error::Object("the encryption dictionary is damaged".into()));
    }
    // UTF-8, at most 127 bytes (SASLprep normalization isn't applied:
    // passwords are rarely anything but plain characters).
    let mut pw = password.as_bytes();
    if pw.len() > 127 {
        let mut end = 127;
        while !password.is_char_boundary(end) {
            end -= 1;
        }
        pw = &pw[..end];
    }
    let hash = |salt: &[u8], extra: &[u8]| if r == 5 { sha256(&[pw, salt, extra].concat()).to_vec() } else { hash_2b(pw, salt, extra) };
    let file_key = |intermediate: Vec<u8>, encrypted: &[u8]| {
        let mut key = encrypted[..32].to_vec();
        Aes::new(&intermediate).cbc_decrypt(&[0; 16], &mut key);
        key
    };
    let u48 = &u[..48];
    if hash(&o[32..40], u48)[..32] == o[..32] {
        return Ok((file_key(hash(&o[40..48], u48), oe), true));
    }
    if hash(&u[32..40], &[])[..32] == u[..32] {
        return Ok((file_key(hash(&u[40..48], &[]), ue), false));
    }
    Err(if password.is_empty() { Error::PasswordNeeded } else { Error::WrongPassword })
}

/// Revision 6's hash (Algorithm 2.B): SHA-256, then rounds of AES-128
/// over the password and the last hash, each hashed with SHA-256, -384 or
/// -512 as its result says, until at least 64 rounds are done and the
/// last byte allows stopping.
fn hash_2b(password: &[u8], salt: &[u8], extra: &[u8]) -> Vec<u8> {
    let mut k = sha256(&[password, salt, extra].concat()).to_vec();
    let mut round = 0usize;
    loop {
        let one: Vec<u8> = [password, &k, extra].concat();
        let mut e: Vec<u8> = one.repeat(64);
        let iv: [u8; 16] = k[16..32].try_into().expect("16 bytes");
        Aes::new(&k[..16]).cbc_encrypt(&iv, &mut e);
        let which = e[..16].iter().map(|&b| b as u32).sum::<u32>() % 3;
        k = match which {
            0 => sha256(&e).to_vec(),
            1 => sha512(&e, true),
            _ => sha512(&e, false),
        };
        round += 1;
        if round >= 64 && (*e.last().expect("bytes") as usize) <= round - 32 {
            break;
        }
    }
    k.truncate(32);
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rc4_known_answer() {
        let mut data = b"Plaintext".to_vec();
        rc4(b"Key", &mut data);
        assert_eq!(data, [0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3]);
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// An encryption dictionary as MuPDF writes it (`mutool clean -E …
    /// -U secret -O boss`).
    fn dict(entries: &[(&str, Object)]) -> Dictionary {
        let mut d = Dictionary::new();
        d.set(b"Filter".to_vec(), Object::Name(b"Standard".to_vec()));
        d.set(b"P".to_vec(), Object::Integer(-4));
        for (k, v) in entries {
            d.set(k.as_bytes().to_vec(), v.clone());
        }
        d
    }

    fn filters(cfm: &str) -> [(&'static str, Object); 3] {
        let mut std_cf = Dictionary::new();
        std_cf.set(b"CFM".to_vec(), Object::Name(cfm.as_bytes().to_vec()));
        let mut cf = Dictionary::new();
        cf.set(b"StdCF".to_vec(), Object::Dictionary(std_cf));
        [("CF", Object::Dictionary(cf)), ("StmF", Object::Name(b"StdCF".to_vec())), ("StrF", Object::Name(b"StdCF".to_vec()))]
    }

    /// The user and the owner password give the same key; others don't
    /// open the file.
    fn check_passwords(d: &Dictionary, id: &[u8]) -> Vec<u8> {
        let user = Crypt::unlock(d, id, "secret").unwrap();
        let owner = Crypt::unlock(d, id, "boss").unwrap();
        assert_eq!(user.key, owner.key);
        assert!(matches!(Crypt::unlock(d, id, "bad"), Err(Error::WrongPassword)));
        assert!(matches!(Crypt::unlock(d, id, ""), Err(Error::PasswordNeeded)));
        user.key
    }

    #[test]
    fn aes_128_passwords() {
        let s = |h: &str| Object::String(hex(h));
        let [cf, stmf, strf] = filters("AESV2");
        let d = dict(&[
            ("V", Object::Integer(4)),
            ("R", Object::Integer(4)),
            ("Length", Object::Integer(128)),
            ("O", s("EFECFCA20761E6E447D16D65E2277349937D0650D26FBA43CCAA77F0EC09B5CD")),
            ("U", s("1641E0056B4C8BA31C852E2095AC15C328BF4E5E4E758A4164004E56FFFA0108")),
            cf,
            stmf,
            strf,
        ]);
        let key = check_passwords(&d, &hex("91A765502349745D3D7B03DABE9355F5"));
        assert_eq!(key.len(), 16);
    }

    #[test]
    fn aes_256_passwords() {
        let s = |h: &str| Object::String(hex(h));
        let [cf, stmf, strf] = filters("AESV3");
        let d = dict(&[
            ("V", Object::Integer(5)),
            ("R", Object::Integer(6)),
            ("O", s("6BDDD7D14F176B451F71D40F6AE5F3716CD95D22C3ED0BADC686A956C41F926A702D23AFE78BAF3428FC5454B32E2BB6")),
            ("U", s("814231511F55E5A2FBFDB8C3AC4E4E5BD7F85127E07C49AA5095A257DE1556C66D152628BD0532861D029EB84538180D")),
            ("OE", s("2E1C8480A3EA5A8BAA6FD86CA6935EC126330D1CA7487F920A7837BFC0504092")),
            ("UE", s("48F5354108ADA01FD47CF4C89AC6214E495C72D222867257F045806C400DEB58")),
            cf,
            stmf,
            strf,
        ]);
        let key = check_passwords(&d, b"");
        // The key decrypts /Perms to the permissions and "adb" (§7.6.4.4.12).
        let mut perms: [u8; 16] = hex("760C133681C78332271D921F5D97DA57").try_into().unwrap();
        Aes::new(&key).decrypt_block(&mut perms);
        assert_eq!(&perms[9..12], b"adb");
        assert_eq!(i32::from_le_bytes(perms[..4].try_into().unwrap()), -4);
    }

    #[test]
    fn aes_round_trip_with_padding() {
        let key = [7u8; 16];
        for len in [0, 5, 16, 33] {
            let data: Vec<u8> = (0..len as u8).collect();
            let sealed = crypt(Method::Aes128, &key, data.clone(), true);
            assert_eq!(sealed.len(), 16 + (len / 16 + 1) * 16);
            assert_eq!(crypt(Method::Aes128, &key, sealed, false), data);
        }
    }
}
