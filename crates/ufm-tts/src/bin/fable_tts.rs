//! fable-tts — Fable's narrator process (Ebook P3).
//!
//! A deliberately small program: JSON lines on stdin, framed PCM
//! on stdout (see `ufm_tts::protocol`). It exists as a process of
//! its own for three reasons:
//!  - Lazy by construction: the engine's ~1.3 GB exists exactly
//!    while this process does. The app spawns it on first Listen
//!    and kills it on stop/idle; there is nothing to "unload".
//!  - Crash-proofing: sherpa-rs `KokoroTts::new` swallows creation
//!    failure into a null session that segfaults on first use
//!    (Phase 0 spike). Every path is validated BEFORE the session
//!    is constructed, and even a runtime death here can never take
//!    the app down — the client reports it as a value.
//!  - Link isolation: this binary is the only artifact that links
//!    sherpa-onnx; the app binary never does.
//!
//! One session at a time, built for the lexicon the current voice
//! needs and rebuilt when a request crosses the US/GB line.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use sherpa_rs::tts::{CommonTtsConfig, KokoroTts, KokoroTtsConfig};
use sherpa_rs::OnnxConfig;

use ufm_tts::model::missing_paths;
use ufm_tts::protocol::{decode_request, encode_response, Request, Response};
use ufm_tts::voices::Lexicon;

struct Args {
    model_dir: PathBuf,
    threads: i32,
}

fn parse_args() -> Result<Args, String> {
    let argv: Vec<String> = std::env::args().collect();
    let mut model_dir: Option<PathBuf> = None;
    let mut threads: i32 = 4;
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--model-dir" => {
                i += 1;
                model_dir = argv.get(i).map(PathBuf::from);
            }
            "--threads" => {
                i += 1;
                threads = argv
                    .get(i)
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| "--threads needs a number".to_string())?;
            }
            other => return Err(format!("unknown argument {other}")),
        }
        i += 1;
    }
    Ok(Args {
        model_dir: model_dir.ok_or_else(|| "--model-dir is required".to_string())?,
        threads,
    })
}

fn build_session(model_dir: &std::path::Path, threads: i32, lexicon: Lexicon) -> KokoroTts {
    let dir = model_dir.to_string_lossy().into_owned();
    let lexicon_file = match lexicon {
        Lexicon::Us => format!("{dir}/lexicon-us-en.txt"),
        Lexicon::Gb => format!("{dir}/lexicon-gb-en.txt"),
    };
    KokoroTts::new(KokoroTtsConfig {
        model: format!("{dir}/model.onnx"),
        voices: format!("{dir}/voices.bin"),
        tokens: format!("{dir}/tokens.txt"),
        // v1.12.9 contract (Phase 0): data_dir IS the espeak-ng-data
        // dir itself (it must hold `phontab`); the multi-lang model
        // also requires lexicon + dict_dir or frontend init fails.
        data_dir: format!("{dir}/espeak-ng-data"),
        dict_dir: format!("{dir}/dict"),
        lexicon: lexicon_file,
        length_scale: 1.0,
        lang: "en".to_string(),
        onnx_config: OnnxConfig {
            provider: "cpu".to_string(),
            debug: false,
            num_threads: threads,
        },
        common_config: CommonTtsConfig {
            rule_fars: String::new(),
            rule_fsts: String::new(),
            max_num_sentences: 1,
            silence_scale: 0.2,
        },
    })
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            println!("{}", encode_response(&Response::fatal(e)));
            std::process::exit(2);
        }
    };
    // Pre-validate EVERYTHING before constructing a session: a
    // missing path must be a fatal line, never a segfault.
    let missing = missing_paths(&args.model_dir);
    if !missing.is_empty() {
        println!(
            "{}",
            encode_response(&Response::fatal(format!(
                "model dir {} lacks {}",
                args.model_dir.display(),
                missing.join(", ")
            )))
        );
        let _ = std::io::stdout().flush();
        std::process::exit(2);
    }

    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let stdout = std::io::stdout();
    let mut session: Option<(Lexicon, KokoroTts)> = None;

    while let Some(line) = lines.next() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let req = match decode_request(&line) {
            Ok(r) => r,
            Err(_) => continue,
        };
        match req {
            Request::Shutdown => break,
            Request::Synth {
                id,
                text,
                sid,
                speed,
                lexicon,
            } => {
                if session.as_ref().map(|(l, _)| *l) != Some(lexicon) {
                    session = None; // drop before rebuild: one model in RAM, ever
                    session = Some((lexicon, build_session(&args.model_dir, args.threads, lexicon)));
                }
                let tts = &mut session.as_mut().expect("session just built").1;
                match tts.create(&text, sid, speed) {
                    Ok(audio) => {
                        let mut out = stdout.lock();
                        let header = encode_response(&Response::audio(
                            id,
                            audio.sample_rate,
                            audio.samples.len(),
                        ));
                        if out.write_all(header.as_bytes()).is_err()
                            || out.write_all(b"\n").is_err()
                        {
                            break;
                        }
                        let mut bytes = Vec::with_capacity(audio.samples.len() * 4);
                        for s in &audio.samples {
                            bytes.extend_from_slice(&s.to_le_bytes());
                        }
                        if out.write_all(&bytes).is_err() || out.flush().is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let mut out = stdout.lock();
                        let line = encode_response(&Response::error(id, format!("{e}")));
                        if out.write_all(line.as_bytes()).is_err()
                            || out.write_all(b"\n").is_err()
                            || out.flush().is_err()
                        {
                            break;
                        }
                    }
                }
            }
        }
    }
    // stdin closed or Shutdown: exit cleanly.
}
