//! The `/update/v6` request of the OPPO / OnePlus / realme update servers.
//!
//! Ported from OplusUpdater by Houvven, as kept up to date in the
//! RealmeOS/OplusUpdater-android fork (`updater/Updater.kt`), GPL-3.0.
//!
//! - `POST /crypto/cert/upgrade` with `{"biz":"ota"}` gives the server's
//!   encryption certificate and its version.
//! - A fresh AES-128 key and IV per request; the JSON body is sent as
//!   `{"params":"{\"cipher\":…,\"iv\":…}"}`, AES-CTR without padding.
//! - The key travels in the `x-otci-cipherInfo` header, wrapped with the
//!   certificate's RSA key (OAEP, SHA-1).
//! - The answer's `body` is the same envelope under the same key.
//!
//! Headers that describe the asking phone (`romVersion`, `androidVersion`, …)
//! stay "unknown": the target is only `otaVersion` + `model`, and real values
//! of another phone make the server answer "nothing found".

use std::time::{SystemTime, UNIX_EPOCH};

use aes::cipher::{KeyIvInit, StreamCipher};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use rand::RngCore;
use rsa::pkcs8::DecodePublicKey;
use rsa::{Oaep, RsaPublicKey};
use serde_json::{Value, json};
use tracing::debug;

use crate::http;
use crate::regions::Region;

type AesCtr = ctr::Ctr128BE<aes::Aes128>;

/// The device id a phone sends: SHA-256 of its IMEI, in capitals. The
/// server keeps one session per device id — concurrent requests under one id
/// fail with 500 — so each searching thread asks as device `n` of its own.
pub fn device_id(n: u32) -> String {
    use sha2::{Digest, Sha256};
    // An IMEI-shaped number with a valid Luhn check digit.
    let body = format!("86{:012}", 1_000_000 + u64::from(n));
    let sum: u32 = body
        .bytes()
        .enumerate()
        .map(|(i, b)| {
            let d = u32::from(b - b'0');
            if i % 2 == 1 {
                let d = d * 2;
                if d > 9 { d - 9 } else { d }
            } else {
                d
            }
        })
        .sum();
    let imei = format!("{body}{}", (10 - sum % 10) % 10);
    Sha256::digest(imei.as_bytes())
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect()
}

/// How the phone asks: by hand, on its own, or for a beta ("taste") build.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Manual,
    ServerAuto,
    ClientAuto,
    Taste,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Manual => "manual",
            Mode::ServerAuto => "server_auto",
            Mode::ClientAuto => "client_auto",
            Mode::Taste => "taste",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Query {
    /// The build the server compares against, e.g. `CPH2747_11.A` or a full
    /// `CPH2747_11.A.48_0480_202609111034`.
    pub ota_version: String,
    /// Defaults to the part of `ota_version` before the first `_`, with the
    /// region's suffix (`Region::model`).
    pub model: Option<String>,
    pub region: Region,
    pub mode: Mode,
}

#[derive(Debug)]
pub enum Answer {
    /// The decrypted build description.
    Build(Value),
    /// Nothing for this model in this region and channel.
    Nothing,
    /// Any other answer, e.g. 304 "security patch has problem".
    Refused { code: i64, message: String },
}

/// A short `ota_version` ("CPH2747_11.A") gets the lowest possible build
/// appended, so that every published build counts as newer.
pub fn full_ota_version(ota_version: &str) -> String {
    if ota_version.split('_').count() < 3 || ota_version.split('.').count() < 3 {
        format!("{ota_version}.01_0001_197001010000")
    } else {
        ota_version.to_string()
    }
}

/// The server's encryption key; one per host serves any number of queries.
#[derive(Clone, Debug)]
pub struct Cert {
    key: RsaPublicKey,
    version: i64,
}

pub fn query(q: &Query) -> Result<Answer> {
    let cert = fetch_cert(q.region.host())?;
    query_with(q, &cert)
}

/// `query` with a certificate fetched earlier from `q.region.host()`.
pub fn query_with(q: &Query, cert: &Cert) -> Result<Answer> {
    query_on(q.region.host(), q, cert, &device_id(0))
}

/// `query` sent to `host`, with that host's certificate. Any host answers
/// any region outside China: the carrier picks the build. Concurrent
/// requests need a `device` id each (see `device_id`).
pub fn query_on(host: &str, q: &Query, cert: &Cert, device: &str) -> Result<Answer> {
    let mut key = [0u8; 16];
    let mut iv = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut key);
    rand::thread_rng().fill_bytes(&mut iv);
    let protected_key = wrap_key(&cert.key, &key)?;

    let body = json!({ "params": seal(&request_body(now_ms(), device).to_string(), &key, &iv) });
    let mut hdrs = headers(q, &protected_key, cert.version, now_ms(), device);
    hdrs.push(("Content-Type", "application/json; charset=utf-8".into()));
    let text = http::post(
        &format!("https://{host}/update/v6"),
        &hdrs,
        &body.to_string(),
    )?;
    parse_answer(&text, &key)
}

/// Turns the server's reply into an answer, decrypting the body with `key`.
pub fn parse_answer(text: &str, key: &[u8; 16]) -> Result<Answer> {
    let root: Value = serde_json::from_str(text)
        .with_context(|| format!("not JSON: {}", text.chars().take(200).collect::<String>()))?;
    let code = match &root["responseCode"] {
        Value::Number(n) => n.as_i64().unwrap_or(0),
        Value::String(s) => s.parse().unwrap_or(0),
        _ => 0,
    };
    let message = root["errMsg"].as_str().unwrap_or_default().to_string();
    debug!("update server: {code} {message}");
    match code {
        200 => {
            let body = match &root["body"] {
                Value::String(s) => open(s, key)?,
                other => other.clone(),
            };
            Ok(Answer::Build(body))
        }
        2004 => Ok(Answer::Nothing),
        _ => Ok(Answer::Refused { code, message }),
    }
}

pub fn fetch_cert(host: &str) -> Result<Cert> {
    let text = http::post(
        &format!("https://{host}/crypto/cert/upgrade"),
        &[("Content-Type", "application/json; charset=utf-8".into())],
        r#"{"biz":"ota"}"#,
    )?;
    parse_cert(&text).with_context(|| format!("{host}: unexpected certificate answer"))
}

/// The encryption key and its version from a `/crypto/cert/upgrade` answer.
pub fn parse_cert(text: &str) -> Result<Cert> {
    let root: Value = serde_json::from_str(text)?;
    let data = &root["data"];
    let pem = data["cert4Encrypt"]
        .as_str()
        .ok_or_else(|| anyhow!("no cert4Encrypt"))?;
    let version = match &data["version"] {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
    .ok_or_else(|| anyhow!("no certificate version"))?;
    let b64: String = pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .flat_map(|l| l.chars().filter(|c| !c.is_whitespace()))
        .collect();
    let der = B64.decode(b64).context("certificate is not base64")?;
    let spki = subject_public_key_info(&der).ok_or_else(|| anyhow!("malformed certificate"))?;
    let key = RsaPublicKey::from_public_key_der(spki).context("not an RSA key")?;
    Ok(Cert { key, version })
}

/// Walks Certificate → TBSCertificate → SubjectPublicKeyInfo; enough DER for
/// one certificate, without an X.509 parser.
fn subject_public_key_info(cert: &[u8]) -> Option<&[u8]> {
    let (_, cert_body, _) = element(cert)?;
    let (_, mut tbs, _) = element(cert_body)?;
    // version [0] EXPLICIT is optional
    if tbs.first() == Some(&0xA0) {
        tbs = element(tbs)?.2;
    }
    // serialNumber, signature, issuer, validity, subject
    for _ in 0..5 {
        tbs = element(tbs)?.2;
    }
    let (whole, _, _) = element(tbs)?;
    Some(whole)
}

/// Splits off the first DER element: (its full encoding, its content, the rest).
fn element(buf: &[u8]) -> Option<(&[u8], &[u8], &[u8])> {
    let first = *buf.get(1)? as usize;
    let (len, header) = if first & 0x80 == 0 {
        (first, 2)
    } else {
        let n = first & 0x7F;
        if n == 0 || n > 4 {
            return None;
        }
        let len = buf
            .get(2..2 + n)?
            .iter()
            .fold(0usize, |acc, b| (acc << 8) | *b as usize);
        (len, 2 + n)
    };
    let end = header.checked_add(len)?;
    Some((buf.get(..end)?, buf.get(header..end)?, buf.get(end..)?))
}

fn wrap_key(public_key: &RsaPublicKey, key: &[u8; 16]) -> Result<String> {
    let wrapped = public_key
        .encrypt(&mut rand::thread_rng(), Oaep::new::<sha1::Sha1>(), key)
        .context("could not encrypt the session key")?;
    Ok(B64.encode(wrapped))
}

fn ctr(data: &mut [u8], key: &[u8; 16], iv: &[u8; 16]) {
    AesCtr::new(key.into(), iv.into()).apply_keystream(data);
}

/// The `{"cipher","iv"}` envelope, as a JSON string.
pub fn seal(plain: &str, key: &[u8; 16], iv: &[u8; 16]) -> String {
    let mut data = plain.as_bytes().to_vec();
    ctr(&mut data, key, iv);
    json!({ "cipher": B64.encode(data), "iv": B64.encode(iv) }).to_string()
}

pub fn open(envelope: &str, key: &[u8; 16]) -> Result<Value> {
    let env: Value = serde_json::from_str(envelope).context("body is not an envelope")?;
    let (Some(cipher), Some(iv)) = (env["cipher"].as_str(), env["iv"].as_str()) else {
        bail!("body has no cipher");
    };
    let mut data = B64.decode(cipher)?;
    let iv: [u8; 16] = B64
        .decode(iv)?
        .try_into()
        .map_err(|_| anyhow!("IV is not 16 bytes"))?;
    ctr(&mut data, key, &iv);
    serde_json::from_slice(&data).context("decrypted body is not JSON")
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// The same for every phone: an empty `components` asks for the full package.
fn request_body(time_ms: i64, device: &str) -> Value {
    json!({
        "components": [],
        "mode": 0,
        "time": time_ms,
        "isSuportPki": false,
        "isRooted": "0",
        "isLocked": false,
        "type": "1",
        "registrationId": "",
        "securityPatch": "",
        "securityPatchVendor": "",
        "strategyVersion": "9",
        "customizeName": "",
        "customizeType": "",
        "supportDLTaste": "1",
        "cota": { "cotaVersion": "", "cotaVersionName": "", "buildType": "user" },
        "opex": { "check": false },
        "sota": {
            "sotaProtocolVersion": "2",
            "sotaProtocolVersionNew": ["apk", "opex", "rus", "apex"],
            "sotaVersion": "new",
            "otaUpdateTime": -1,
            "updateViaReboot": 2,
            "frameworkVer": "10",
            "supportLightH": "1"
        },
        "otaAppVersion": 0,
        "deviceId": device,
        "duid": "",
        "h5LinkVersion": 7,
        "canUpgVer": "svt",
        "cacheVersion": {
            "id": "",
            "otaVersion": "",
            "checkOtaVersionTime": -1,
            "sotaVersion": "",
            "checkSotaVersionTime": -1,
            "tasteSotaVersion": ""
        }
    })
}

fn headers(
    q: &Query,
    protected_key: &str,
    cert_version: i64,
    time_ms: i64,
    device: &str,
) -> Vec<(&'static str, String)> {
    let ota_version = full_ota_version(&q.ota_version);
    let model = q.model.clone().unwrap_or_else(|| {
        q.region
            .model(ota_version.split('_').next().unwrap_or_default())
    });
    // Scene version: a day ahead, in 100 ns ticks.
    let scene_version = (time_ms + 24 * 3_600_000) * 10_000;
    let cipher_info = json!({
        "ota": {
            "protectedKey": protected_key,
            "version": scene_version,
            "certVersion": cert_version
        }
    });
    let lang = q.region.language().to_string();
    vec![
        ("language", lang.clone()),
        ("newLanguage", lang),
        ("romVersion", "unknown".into()),
        ("androidVersion", "unknown".into()),
        ("colorOSVersion", "unknown".into()),
        ("infVersion", "1".into()),
        ("otaVersion", ota_version),
        ("model", model),
        ("mode", q.mode.as_str().into()),
        ("nvCarrier", q.region.carrier().into()),
        ("pipelineKey", "ALLNET".into()),
        ("operator", "ALLNET".into()),
        ("companyId", String::new()),
        ("rsaVersion", "3".into()),
        ("version", "3".into()),
        ("deviceId", device.into()),
        ("x-otci-deviceId", device.into()),
        ("x-otci-cipherInfo", cipher_info.to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::traits::PublicKeyParts;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn device_ids_look_like_a_phones() {
        let a = device_id(0);
        assert_eq!(a.len(), 64);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
        );
        assert_ne!(a, device_id(1));
        assert_eq!(a, device_id(0));
    }

    #[test]
    fn short_ota_versions_get_the_lowest_build() {
        assert_eq!(
            full_ota_version("CPH2747_11.A"),
            "CPH2747_11.A.01_0001_197001010000"
        );
        let full = "CPH2747_11.A.48_0480_202609111034";
        assert_eq!(full_ota_version(full), full);
    }

    #[test]
    fn ctr_matches_nist_sp800_38a() {
        // F.5.1 CTR-AES128.Encrypt, first block
        let key: [u8; 16] = hex("2b7e151628aed2a6abf7158809cf4f3c").try_into().unwrap();
        let iv: [u8; 16] = hex("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff").try_into().unwrap();
        let mut data = hex("6bc1bee22e409f96e93d7e117393172a");
        ctr(&mut data, &key, &iv);
        assert_eq!(data, hex("874d6191b620e3261bef6864990db6ce"));
    }

    #[test]
    fn envelope_round_trips() {
        let key = [7u8; 16];
        let iv = [9u8; 16];
        let plain = request_body(1_700_000_000_000, "D").to_string();
        let sealed = seal(&plain, &key, &iv);
        assert_eq!(
            open(&sealed, &key).unwrap(),
            request_body(1_700_000_000_000, "D")
        );
    }

    #[test]
    fn reads_the_server_certificate() {
        let cert = parse_cert(include_str!("../testdata/cert-sg.json")).unwrap();
        assert_eq!(cert.key.size(), 256);
        assert!(cert.version > 0);
        let wrapped = B64
            .decode(wrap_key(&cert.key, &[1u8; 16]).unwrap())
            .unwrap();
        assert_eq!(wrapped.len(), 256);
    }

    #[test]
    fn answers_by_response_code() {
        let key = [3u8; 16];
        let body = seal(r#"{"otaVersion":"X_11.A.48"}"#, &key, &[4u8; 16]);
        let ok = json!({ "responseCode": 200, "body": body }).to_string();
        match parse_answer(&ok, &key).unwrap() {
            Answer::Build(b) => assert_eq!(b["otaVersion"], "X_11.A.48"),
            other => panic!("{other:?}"),
        }
        let none =
            r#"{"responseCode":2004,"errMsg":"artifactV1Result is empty","body":{"paramFlag":0}}"#;
        assert!(matches!(parse_answer(none, &key).unwrap(), Answer::Nothing));
        let refused = r#"{"responseCode":304,"errMsg":"security patch has problem"}"#;
        assert!(matches!(
            parse_answer(refused, &key).unwrap(),
            Answer::Refused { code: 304, .. }
        ));
    }

    #[test]
    fn headers_name_the_target_only() {
        let q = Query {
            ota_version: "CPH2747_11.A".into(),
            model: None,
            region: Region::GL,
            mode: Mode::Manual,
        };
        let h = headers(&q, "KEY", 42, 1_000, "DEV");
        let get = |n: &str| h.iter().find(|(k, _)| *k == n).unwrap().1.clone();
        assert_eq!(get("model"), "CPH2747");
        assert_eq!(get("otaVersion"), "CPH2747_11.A.01_0001_197001010000");
        assert_eq!(get("nvCarrier"), "10100111");
        assert_eq!(get("romVersion"), "unknown");
        let info: Value = serde_json::from_str(&get("x-otci-cipherInfo")).unwrap();
        assert_eq!(info["ota"]["certVersion"], 42);
        assert_eq!(info["ota"]["version"], (1_000 + 86_400_000) * 10_000i64);
    }
}
