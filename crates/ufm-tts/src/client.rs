//! The client half of the helper protocol: spawn `fable-tts`,
//! speak JSON at it, read PCM back. Blocking by design — callers
//! run it on a dedicated synthesis thread, never the UI thread and
//! never the playback clock's thread.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use crate::protocol::{decode_response, encode_request, Request};
use crate::voices::Voice;
use crate::{Audio, TtsEngine, TtsError};

pub struct HelperEngine {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl HelperEngine {
    /// Spawn the helper against an already-downloaded model dir.
    /// The model dir is validated BEFORE the spawn (a missing file
    /// must be a `ModelMissing` value here, not a dead helper
    /// there).
    pub fn spawn(helper: &Path, model_dir: &Path, threads: i32) -> Result<Self, TtsError> {
        if !helper.is_file() {
            return Err(TtsError::HelperMissing(format!(
                "no narrator program at {}",
                helper.display()
            )));
        }
        let missing = crate::model::missing_paths(model_dir);
        if !missing.is_empty() {
            return Err(TtsError::ModelMissing(format!(
                "{} lacks {}",
                model_dir.display(),
                missing.join(", ")
            )));
        }
        let mut child = Command::new(helper)
            .arg("--model-dir")
            .arg(model_dir)
            .arg("--threads")
            .arg(threads.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| TtsError::Spawn(format!("{}: {e}", helper.display())))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| TtsError::Spawn("helper has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| TtsError::Spawn("helper has no stdout".into()))?;
        Ok(HelperEngine {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    fn read_line(&mut self) -> Result<String, TtsError> {
        let mut line = String::new();
        let n = self
            .stdout
            .read_line(&mut line)
            .map_err(|e| TtsError::Protocol(format!("reading from the helper: {e}")))?;
        if n == 0 {
            return Err(TtsError::Protocol(
                "the helper closed its side of the conversation".into(),
            ));
        }
        Ok(line)
    }
}

impl TtsEngine for HelperEngine {
    fn synthesize(&mut self, text: &str, voice: &Voice, speed: f32) -> Result<Audio, TtsError> {
        let id = self.next_id;
        self.next_id += 1;
        let req = Request::Synth {
            id,
            text: text.to_string(),
            sid: voice.sid,
            speed,
            lexicon: voice.lexicon,
        };
        let line = encode_request(&req);
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.write_all(b"\n"))
            .and_then(|()| self.stdin.flush())
            .map_err(|e| TtsError::Protocol(format!("writing to the helper: {e}")))?;
        let header = self.read_line()?;
        let resp = decode_response(header.trim()).map_err(TtsError::Protocol)?;
        if let Some(fatal) = resp.fatal {
            return Err(TtsError::Protocol(format!("helper refused: {fatal}")));
        }
        if resp.id != Some(id) {
            return Err(TtsError::Protocol(format!(
                "helper answered id {:?}, expected {id}",
                resp.id
            )));
        }
        if let Some(err) = resp.error {
            return Err(TtsError::Synth(err));
        }
        let count = resp
            .samples
            .ok_or_else(|| TtsError::Protocol("audio header without a sample count".into()))?;
        let sample_rate = resp
            .sample_rate
            .ok_or_else(|| TtsError::Protocol("audio header without a sample rate".into()))?;
        let mut bytes = vec![0u8; count * 4];
        self.stdout
            .read_exact(&mut bytes)
            .map_err(|e| TtsError::Protocol(format!("reading PCM from the helper: {e}")))?;
        let samples = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        Ok(Audio {
            samples,
            sample_rate,
        })
    }

    fn shutdown(&mut self) {
        let line = encode_request(&Request::Shutdown);
        let _ = self.stdin.write_all(line.as_bytes());
        let _ = self.stdin.write_all(b"\n");
        let _ = self.stdin.flush();
        // A polite moment to exit, then insist.
        for _ in 0..20 {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                _ => std::thread::sleep(std::time::Duration::from_millis(25)),
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for HelperEngine {
    fn drop(&mut self) {
        // Never leave a 1.3 GB narrator standing around.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
