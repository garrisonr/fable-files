//! The helper wire protocol: JSON lines in, a JSON header line +
//! raw PCM out. Deliberately tiny — the helper is a narrator, not
//! a server.
//!
//! In (stdin, one JSON object per line):
//!   {"cmd":"synth","id":7,"text":"…","sid":3,"speed":1.0,"lexicon":"us"}
//!   {"cmd":"shutdown"}
//! Out (stdout):
//!   {"id":7,"sample_rate":24000,"samples":12345}\n<12345 × f32 LE>
//!   {"id":7,"error":"…"}\n
//!   {"fatal":"…"}\n            (startup validation failed; exiting)

use serde::{Deserialize, Serialize};

use crate::voices::Lexicon;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Synth {
        id: u64,
        text: String,
        sid: i32,
        speed: f32,
        lexicon: Lexicon,
    },
    Shutdown,
}

/// The header line that precedes one PCM frame (or reports why
/// there isn't one).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fatal: Option<String>,
}

impl Response {
    pub fn audio(id: u64, sample_rate: u32, samples: usize) -> Self {
        Response {
            id: Some(id),
            sample_rate: Some(sample_rate),
            samples: Some(samples),
            error: None,
            fatal: None,
        }
    }
    pub fn error(id: u64, message: String) -> Self {
        Response {
            id: Some(id),
            sample_rate: None,
            samples: None,
            error: Some(message),
            fatal: None,
        }
    }
    pub fn fatal(message: String) -> Self {
        Response {
            id: None,
            sample_rate: None,
            samples: None,
            error: None,
            fatal: Some(message),
        }
    }
}

pub fn encode_request(req: &Request) -> String {
    serde_json::to_string(req).expect("request serialization cannot fail")
}

pub fn decode_request(line: &str) -> Result<Request, String> {
    serde_json::from_str(line).map_err(|e| format!("bad request line: {e}"))
}

pub fn encode_response(resp: &Response) -> String {
    serde_json::to_string(resp).expect("response serialization cannot fail")
}

pub fn decode_response(line: &str) -> Result<Response, String> {
    serde_json::from_str(line).map_err(|e| format!("bad response line: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trip() {
        let req = Request::Synth {
            id: 7,
            text: "The train was an hour late.".into(),
            sid: 3,
            speed: 1.25,
            lexicon: Lexicon::Gb,
        };
        let line = encode_request(&req);
        assert_eq!(decode_request(&line).unwrap(), req);
        assert_eq!(
            decode_request(&encode_request(&Request::Shutdown)).unwrap(),
            Request::Shutdown
        );
    }

    #[test]
    fn response_shapes() {
        let a = decode_response(&encode_response(&Response::audio(3, 24000, 10))).unwrap();
        assert_eq!(a.samples, Some(10));
        let e = decode_response(&encode_response(&Response::error(3, "no".into()))).unwrap();
        assert_eq!(e.error.as_deref(), Some("no"));
        let f = decode_response(&encode_response(&Response::fatal("gone".into()))).unwrap();
        assert_eq!(f.fatal.as_deref(), Some("gone"));
    }
}
